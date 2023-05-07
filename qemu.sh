# ./tools/qemu/build/qemu-system-riscv32 -M sifive_e,revb=true -kernel ./target/riscv32imac-unknown-none-elf/release/hifive1.elf -nographic
#!/bin/sh
 
git filter-branch --env-filter '
 
OLD_EMAIL="luka.kvavilashvili@deckers.com"
CORRECT_NAME="Luka Kvavilashvili"
CORRECT_EMAIL="lukakvavilashvili@gmail.com"

if [ "$GIT_COMMITTER_EMAIL" = "$OLD_EMAIL" ]
then
    export GIT_COMMITTER_NAME="$CORRECT_NAME"
    export GIT_COMMITTER_EMAIL="$CORRECT_EMAIL"
fi
if [ "$GIT_AUTHOR_EMAIL" = "$OLD_EMAIL" ]
then
    export GIT_AUTHOR_NAME="$CORRECT_NAME"
    export GIT_AUTHOR_EMAIL="$CORRECT_EMAIL"
fi
' --tag-name-filter cat -- --branches --tags