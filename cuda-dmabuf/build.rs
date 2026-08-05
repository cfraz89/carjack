// Link the native libraries that cuda-dmabuf's FFI functions need.
//
// Only when the `cuda` feature is enabled (forwarded from the workspace's `cuda` feature), so the
// crate compiles on non-NVIDIA systems without them. Install the required -devel packages in the
// build toolbox when enabling it:
//   - libEGL-devel        (provides libEGL.so)
//   - libgbm-devel        (provides libgbm.so)
//   - gstreamer1-plugin-cuda-devel  (provides libgstcuda-1.0.so)
fn main() {
    if std::env::var_os("CARGO_FEATURE_CUDA").is_some() {
        println!("cargo:rustc-link-lib=cuda");
        println!("cargo:rustc-link-lib=EGL");
        println!("cargo:rustc-link-lib=gbm");
        println!("cargo:rustc-link-lib=gstcuda-1.0");
    }
}
