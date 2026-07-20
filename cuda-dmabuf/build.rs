// Link the native libraries that cuda-dmabuf's FFI functions need.
//
// Install the required -devel packages in the build toolbox:
//   - libEGL-devel        (provides libEGL.so)
//   - libgbm-devel        (provides libgbm.so)
//   - gstreamer1-plugin-cuda-devel  (provides libgstcuda-1.0.so)
fn main() {
    println!("cargo:rustc-link-lib=cuda");
    println!("cargo:rustc-link-lib=EGL");
    println!("cargo:rustc-link-lib=gbm");
    println!("cargo:rustc-link-lib=gstcuda-1.0");
}
