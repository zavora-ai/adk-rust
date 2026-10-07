//! Seccomp program that denies process creation, loaded through bubblewrap's `--seccomp`.
//!
//! The program is classic BPF in the `struct sock_filter` layout bubblewrap reads from a file
//! descriptor. It makes `fork`, `vfork`, and every `clone` without `CLONE_THREAD` fail with
//! `EPERM`, and `clone3` fail with `ENOSYS`: its flags live in user memory that seccomp cannot
//! inspect, and `ENOSYS` makes libc fall back to `clone` when it creates a thread. Threads and
//! `execve` keep working. Syscalls entering through any other ABI — 32-bit compat or x32 —
//! fail with `EPERM`, so they cannot bypass the syscall-number checks.
//!
//! The program is pure data, so it is built and unit-tested on every host; only the Linux
//! enforcer loads it.

/// One `struct sock_filter` instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Instruction {
    /// Opcode.
    pub(crate) code: u16,
    /// Jump offset when the condition holds.
    pub(crate) jt: u8,
    /// Jump offset when the condition does not hold.
    pub(crate) jf: u8,
    /// Constant operand.
    pub(crate) k: u32,
}

/// The syscall numbers one Linux ABI uses for process creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Abi {
    /// `AUDIT_ARCH_*` value the kernel reports in `seccomp_data.arch`.
    pub(crate) audit_arch: u32,
    /// `fork`, where the ABI has one.
    pub(crate) fork: Option<u32>,
    /// `vfork`, where the ABI has one.
    pub(crate) vfork: Option<u32>,
    /// `clone`. Its first argument is the flags word on every supported ABI.
    pub(crate) clone: u32,
    /// `clone3`.
    pub(crate) clone3: u32,
    /// Syscall numbers at or above this value belong to another ABI sharing the audit arch.
    pub(crate) foreign_syscall_floor: Option<u32>,
}

/// x86-64. Numbers at or above `__X32_SYSCALL_BIT` are x32 syscalls under the same audit arch.
pub(crate) const X86_64: Abi = Abi {
    audit_arch: 0xc000_003e,
    fork: Some(57),
    vfork: Some(58),
    clone: 56,
    clone3: 435,
    foreign_syscall_floor: Some(0x4000_0000),
};

/// AArch64, which has no `fork` or `vfork` syscall.
pub(crate) const AARCH64: Abi = Abi {
    audit_arch: 0xc000_00b7,
    fork: None,
    vfork: None,
    clone: 220,
    clone3: 435,
    foreign_syscall_floor: None,
};

impl Abi {
    /// The ABI of the architecture this crate was compiled for, when supported.
    #[cfg(all(feature = "sandbox-linux", target_os = "linux"))]
    pub(crate) fn native() -> Option<&'static Abi> {
        if cfg!(target_arch = "x86_64") {
            Some(&X86_64)
        } else if cfg!(target_arch = "aarch64") {
            Some(&AARCH64)
        } else {
            None
        }
    }
}

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_JMP_JGE_K: u16 = 0x35;
const BPF_JMP_JSET_K: u16 = 0x45;
const BPF_RET_K: u16 = 0x06;

const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const EPERM: u32 = 1;
const ENOSYS: u32 = 38;
const CLONE_THREAD: u32 = 0x0001_0000;

/// `seccomp_data` field offsets. The low word of `args[0]` is at offset 16 on the
/// little-endian ABIs supported here.
const OFFSET_NR: u32 = 0;
const OFFSET_ARCH: u32 = 4;
const OFFSET_ARG0_LOW: u32 = 16;

/// Where a conditional jump lands before offsets are resolved.
#[derive(Debug, Clone, Copy)]
enum Target {
    Next,
    Allow,
    Deny,
    NoSys,
}

/// Builds the program that denies process creation under `abi`.
pub(crate) fn deny_process_creation(abi: &Abi) -> Vec<Instruction> {
    let mut body: Vec<(u16, u32, Target, Target)> = vec![
        (BPF_LD_W_ABS, OFFSET_ARCH, Target::Next, Target::Next),
        (BPF_JMP_JEQ_K, abi.audit_arch, Target::Next, Target::Deny),
        (BPF_LD_W_ABS, OFFSET_NR, Target::Next, Target::Next),
    ];
    if let Some(floor) = abi.foreign_syscall_floor {
        body.push((BPF_JMP_JGE_K, floor, Target::Deny, Target::Next));
    }
    for number in [abi.fork, abi.vfork].into_iter().flatten() {
        body.push((BPF_JMP_JEQ_K, number, Target::Deny, Target::Next));
    }
    body.push((BPF_JMP_JEQ_K, abi.clone3, Target::NoSys, Target::Next));
    body.push((BPF_JMP_JEQ_K, abi.clone, Target::Next, Target::Allow));
    body.push((BPF_LD_W_ABS, OFFSET_ARG0_LOW, Target::Next, Target::Next));
    body.push((BPF_JMP_JSET_K, CLONE_THREAD, Target::Allow, Target::Deny));

    let allow = body.len();
    let resolve = |index: usize, target: Target| -> u8 {
        let destination = match target {
            Target::Next => index + 1,
            Target::Allow => allow,
            Target::Deny => allow + 1,
            Target::NoSys => allow + 2,
        };
        // The program is a few dozen instructions, far inside a jump offset's range.
        u8::try_from(destination - (index + 1)).expect("jump offset fits in u8")
    };

    let mut program: Vec<Instruction> = body
        .iter()
        .enumerate()
        .map(|(index, &(code, k, jt, jf))| Instruction {
            code,
            jt: resolve(index, jt),
            jf: resolve(index, jf),
            k,
        })
        .collect();
    for k in [SECCOMP_RET_ALLOW, SECCOMP_RET_ERRNO | EPERM, SECCOMP_RET_ERRNO | ENOSYS] {
        program.push(Instruction { code: BPF_RET_K, jt: 0, jf: 0, k });
    }
    program
}

/// Serializes `program` as the `struct sock_filter` array bubblewrap expects.
pub(crate) fn encode(program: &[Instruction]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(program.len() * 8);
    for instruction in program {
        bytes.extend_from_slice(&instruction.code.to_ne_bytes());
        bytes.push(instruction.jt);
        bytes.push(instruction.jf);
        bytes.extend_from_slice(&instruction.k.to_ne_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUDIT_ARCH_I386: u32 = 0x4000_0003;
    const SIGCHLD: u64 = 17;
    const CLONE_VM: u64 = 0x100;
    const CLONE_VFORK: u64 = 0x4000;
    /// The flags glibc's `pthread_create` passes to `clone`.
    const THREAD_FLAGS: u64 = 0x003d_0f00;

    /// Executes `program` against one syscall, as the kernel's classic-BPF interpreter would
    /// for the opcodes this module emits.
    fn run(program: &[Instruction], arch: u32, nr: u32, arg0: u64) -> u32 {
        // struct seccomp_data { int nr; u32 arch; u64 instruction_pointer; u64 args[6]; }
        let mut data = [0u8; 64];
        data[0..4].copy_from_slice(&nr.to_le_bytes());
        data[4..8].copy_from_slice(&arch.to_le_bytes());
        data[16..24].copy_from_slice(&arg0.to_le_bytes());

        let mut accumulator = 0u32;
        let mut pc = 0usize;
        loop {
            let instruction = program.get(pc).expect("the program ran off its end");
            let k = instruction.k;
            let jump = |taken: bool| {
                pc + 1 + usize::from(if taken { instruction.jt } else { instruction.jf })
            };
            pc = match instruction.code {
                BPF_LD_W_ABS => {
                    let offset = usize::try_from(k).unwrap();
                    accumulator = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                    pc + 1
                }
                BPF_JMP_JEQ_K => jump(accumulator == k),
                BPF_JMP_JGE_K => jump(accumulator >= k),
                BPF_JMP_JSET_K => jump(accumulator & k != 0),
                BPF_RET_K => return k,
                other => panic!("unexpected opcode {other:#x}"),
            };
        }
    }

    const ALLOW: u32 = SECCOMP_RET_ALLOW;
    const DENY: u32 = SECCOMP_RET_ERRNO | EPERM;
    const NOSYS: u32 = SECCOMP_RET_ERRNO | ENOSYS;

    #[test]
    fn x86_64_denies_every_process_creation_path() {
        let program = deny_process_creation(&X86_64);
        let arch = X86_64.audit_arch;

        assert_eq!(run(&program, arch, 57, 0), DENY, "fork");
        assert_eq!(run(&program, arch, 58, 0), DENY, "vfork");
        assert_eq!(run(&program, arch, 56, SIGCHLD), DENY, "clone as fork");
        assert_eq!(run(&program, arch, 56, CLONE_VM | CLONE_VFORK | SIGCHLD), DENY, "posix_spawn");
        assert_eq!(run(&program, arch, 435, 0), NOSYS, "clone3");
    }

    #[test]
    fn x86_64_keeps_threads_exec_and_ordinary_syscalls() {
        let program = deny_process_creation(&X86_64);
        let arch = X86_64.audit_arch;

        assert_eq!(run(&program, arch, 56, THREAD_FLAGS), ALLOW, "pthread_create");
        assert_eq!(run(&program, arch, 59, 0), ALLOW, "execve");
        assert_eq!(run(&program, arch, 39, 0), ALLOW, "getpid");
        assert_eq!(run(&program, arch, 0, 0), ALLOW, "read");
    }

    #[test]
    fn x86_64_rejects_other_abis() {
        let program = deny_process_creation(&X86_64);

        assert_eq!(run(&program, AUDIT_ARCH_I386, 2, 0), DENY, "i386 fork via int 0x80");
        assert_eq!(run(&program, AUDIT_ARCH_I386, 20, 0), DENY, "i386 getpid");
        assert_eq!(run(&program, X86_64.audit_arch, 0x4000_0000 | 57, 0), DENY, "x32 fork");
        assert_eq!(run(&program, X86_64.audit_arch, 0x4000_0000 | 39, 0), DENY, "x32 getpid");
    }

    #[test]
    fn aarch64_denies_process_creation_and_keeps_threads() {
        let program = deny_process_creation(&AARCH64);
        let arch = AARCH64.audit_arch;

        assert_eq!(run(&program, arch, 220, SIGCHLD), DENY, "clone as fork");
        assert_eq!(run(&program, arch, 220, CLONE_VM | CLONE_VFORK | SIGCHLD), DENY, "posix_spawn");
        assert_eq!(run(&program, arch, 435, 0), NOSYS, "clone3");
        assert_eq!(run(&program, arch, 220, THREAD_FLAGS), ALLOW, "pthread_create");
        assert_eq!(run(&program, arch, 221, 0), ALLOW, "execve");
        assert_eq!(run(&program, arch, 172, 0), ALLOW, "getpid");
        assert_eq!(run(&program, X86_64.audit_arch, 172, 0), DENY, "foreign arch");
    }

    #[test]
    fn every_jump_lands_inside_the_program() {
        for abi in [&X86_64, &AARCH64] {
            let program = deny_process_creation(abi);
            for (index, instruction) in program.iter().enumerate() {
                if instruction.code == BPF_RET_K {
                    continue;
                }
                for offset in [instruction.jt, instruction.jf] {
                    assert!(index + 1 + usize::from(offset) < program.len(), "{abi:?} @ {index}");
                }
            }
            assert_eq!(program.last().map(|i| i.code), Some(BPF_RET_K));
        }
    }

    #[test]
    fn encoding_matches_the_sock_filter_layout() {
        let program = [Instruction { code: 0x0015, jt: 1, jf: 2, k: 0xc000_003e }];
        let mut expected = Vec::new();
        expected.extend_from_slice(&0x0015u16.to_ne_bytes());
        expected.extend_from_slice(&[1, 2]);
        expected.extend_from_slice(&0xc000_003eu32.to_ne_bytes());

        assert_eq!(encode(&program), expected);
        assert_eq!(encode(&deny_process_creation(&X86_64)).len() % 8, 0);
    }
}
