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
armbian-build board command: build-armbian-container
    podman run --rm -it -v ./output:/armbian/output:z -v ./cache:/armbian/cache:z armbian-build-carjack ./compile.sh BOARD={{ board }} {{ command }}

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
