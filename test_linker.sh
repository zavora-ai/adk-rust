#!/bin/bash
cat << 'C_EOF' > hello.c
#include <stdio.h>
int main() {
    printf("Hello, world!\n");
    return 0;
}
C_EOF
clang -fuse-ld=mold hello.c -o hello_mold
clang --ld-path=wild hello.c -o hello_wild
echo "mold:"
ls -l hello_mold
echo "wild:"
ls -l hello_wild
./hello_mold
./hello_wild
