mod armbian

run $GST_DEBUG="GST_CAPS:5,pipewiresrc:5":
    cargo run

build:
    cargo build
