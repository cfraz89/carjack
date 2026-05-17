run $GST_DEBUG="GST_CAPS:5,pipewiresrc:5":
    cargo run

armbian-build-kernel board: (armbian-build board "build")

# Useful for generating a patched kernel config
[working-directory('armbian')]
armbian-kernel-config board: (armbian-build board "kernel-config")
    mv output/config/*.config userpatches/

# Useful for generating a kernel or dts patch
[working-directory('armbian')]
armbian-kernel-patch board: (armbian-build board "kernel-patch")

[working-directory('armbian')]
armbian-build board command: chown-armbian-build-volumes build-armbian-container
    #!/usr/bin/env sh
    uid=`id -u`
    gid=`id -g`
    tmpdir="$PWD"/tmp
    sudo mount -t tmpfs -o size=12G,mode=0777,uid="$uid",gid="$gid" tmpfs "$tmpdir"
    podman run --rm -it -v ./output:/armbian/output:z -v ./cache:/armbian/cache:z -v ./tmp/:/armbian/.tmp:z armbian-build-carjack ./compile.sh BOARD={{ board }} {{ command }}
    sudo umount $tmpdir

[working-directory('armbian')]
chown-armbian-build-volumes:
    sudo --preserve-env chown -R $USER:$USER output
    sudo --preserve-env chown -R $USER:$USER cache
    sudo --preserve-env chown -R $USER:$USER tmp

[working-directory('armbian')]
clean-armbian-build:
    sudo rm -rf output/*
    sudo rm -rf cache/*
    sudo rm -rf tmp/*

# [working-directory('armbian')]
# armbian-build board command:
#     if [ ! -d "./build" ]; then echo "Cloning armbian build system"; git clone https://github.com/armbian/build; fi
#     ln -sf  userpatches build/userpatches
#     # won't work in podman, so do beforehand
#     sudo losetup -f
#     build/compile.sh BOARD={{ board }} {{ command }}

[working-directory('armbian')]
build-armbian-container:
    podman build -t armbian-build-carjack .
