#!/bin/bash
cat << 'C_EOF' > hello.c
#include <stdio.h>
int main() {
    printf("Hello, wild!\n");
    return 0;
}
C_EOF
clang --ld-path=wild -Wl,--incremental hello.c -o hello_wild_inc || clang --ld-path=wild -Wl,--wild-incremental hello.c -o hello_wild_inc || echo "wild does not support incremental flag in this way"
