//! carjack's CarPlay core for the Android app. The app owns Bluetooth, WiFi, the screen and
//! input; this runs iAP2, the CarPlay handshake and AirPlay between them. The JNI surface is
//! `io.github.cfraz89.slipstream.Native`.

mod logcat;
pub mod session;

use std::collections::HashMap;
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use jni::objects::{GlobalRef, JByteBuffer, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jint, jlong};
use jni::{JNIEnv, JavaVM};
use libcarjack::airplay::VideoCodec;
use libcarjack::carplay_init::{DeviceIdentity, TouchEvent, TouchEventType, WifiCredentials};

use crate::session::{JoinedWifi, Platform, SessionCtl, SessionParams, VideoConfig};

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("carjack")
            .build()
            .expect("tokio runtime")
    })
}

fn sessions() -> &'static Mutex<HashMap<jlong, Arc<SessionCtl>>> {
    static SESSIONS: OnceLock<Mutex<HashMap<jlong, Arc<SessionCtl>>>> = OnceLock::new();
    SESSIONS.get_or_init(Default::default)
}

fn session(handle: jlong) -> Option<Arc<SessionCtl>> {
    sessions().lock().unwrap().get(&handle).cloned()
}

/// The app's `Native.Callbacks` for one session.
struct JavaPlatform {
    vm: JavaVM,
    callbacks: GlobalRef,
}

impl JavaPlatform {
    fn call(&self, name: &str, sig: &str, args: &[JValue]) {
        let result = self.vm.attach_current_thread_as_daemon().and_then(|mut env| {
            let result = env.call_method(&self.callbacks, name, sig, args);
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_describe();
                let _ = env.exception_clear();
            }
            result
        });
        if let Err(e) = result {
            tracing::warn!("Callback {name} failed: {e}");
        }
    }

    fn call_strings(&self, name: &str, sig: &str, strings: &[Option<&str>]) {
        let Ok(env) = self.vm.attach_current_thread_as_daemon() else {
            return;
        };
        let objects: Vec<JObject> = strings
            .iter()
            .map(|s| match s {
                Some(s) => env.new_string(s).map(JObject::from).unwrap_or_default(),
                None => JObject::null(),
            })
            .collect();
        let args: Vec<JValue> = objects.iter().map(JValue::Object).collect();
        self.call(name, sig, &args);
    }
}

impl Platform for JavaPlatform {
    fn join_wifi(&self, creds: &WifiCredentials) {
        self.call_strings(
            "joinWifi",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            &[
                Some(&creds.ssid),
                creds.passphrase.as_deref(),
                creds.bssid.as_deref(),
            ],
        );
    }

    fn start_video(&self, video: &VideoConfig) {
        let (vx, vy, vw, vh) = video.view;
        self.call(
            "startVideo",
            "(IIIIIIIZ)V",
            &[
                JValue::Int(video.width as jint),
                JValue::Int(video.height as jint),
                JValue::Int(video.fps as jint),
                JValue::Int(vx as jint),
                JValue::Int(vy as jint),
                JValue::Int(vw as jint),
                JValue::Int(vh as jint),
                JValue::Bool(video.codec.is_hevc() as jboolean),
            ],
        );
    }

    fn request_keyframe(&self) {
        self.call("requestKeyframe", "()V", &[]);
    }

    fn touch(&self, event: &TouchEvent) {
        let action = match event.event_type {
            TouchEventType::Down => 0,
            TouchEventType::Up => 1,
            TouchEventType::Motion => 2,
        };
        self.call(
            "touch",
            "(IIFF)V",
            &[
                JValue::Int(action),
                JValue::Int(event.slot as jint),
                JValue::Float(event.x as f32),
                JValue::Float(event.y as f32),
            ],
        );
    }
}

fn string(env: &mut JNIEnv, s: &JString) -> Option<String> {
    if s.is_null() {
        return None;
    }
    env.get_string(s).ok().map(Into::into)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_cfraz89_slipstream_Native_init(
    mut env: JNIEnv,
    _class: JClass,
    homekit_path: JString,
) {
    logcat::init();
    if let Some(path) = string(&mut env, &homekit_path) {
        if let Err(e) = libcarjack::airplay::init_homekit_storage(std::path::Path::new(&path)) {
            tracing::warn!("No HomeKit storage ({e}); pairing from scratch every run");
        }
    }
}

/// Start a session on `iap_fd`, the app's end of its RFCOMM relay; ownership passes here.
/// `codec` is 0 for whatever the accessory implies, 1 for H264, 2 for HEVC.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_cfraz89_slipstream_Native_startSession(
    mut env: JNIEnv,
    _class: JClass,
    callbacks: JObject,
    iap_fd: jint,
    name: JString,
    bluetooth_address: JString,
    codec: jint,
) -> jlong {
    static NEXT: AtomicI64 = AtomicI64::new(1);
    // SAFETY: the app hands over a socket fd it no longer uses.
    let iap = unsafe { std::os::unix::net::UnixStream::from_raw_fd(iap_fd) };
    let (Ok(vm), Ok(callbacks)) = (env.get_java_vm(), env.new_global_ref(callbacks)) else {
        return 0;
    };
    let name = string(&mut env, &name).unwrap_or_else(|| String::from("carjack"));
    let address = string(&mut env, &bluetooth_address).unwrap_or_default();
    let params = SessionParams {
        identity: DeviceIdentity {
            name,
            bluetooth_address: address.to_lowercase(),
        },
        bluetooth_id: address.to_uppercase(),
        codec: match codec {
            1 => Some(VideoCodec::H264),
            2 => Some(VideoCodec::Hevc),
            _ => None,
        },
    };

    let handle = NEXT.fetch_add(1, Ordering::Relaxed);
    let ctl = Arc::new(SessionCtl::default());
    sessions().lock().unwrap().insert(handle, ctl.clone());
    let platform = Arc::new(JavaPlatform { vm, callbacks });

    runtime().spawn(async move {
        let result = async {
            iap.set_nonblocking(true)?;
            let iap = tokio::net::UnixStream::from_std(iap)?;
            tokio::select! {
                result = session::run(platform.clone(), ctl.clone(), iap, params) => result,
                _ = ctl.cancel.cancelled() => Err(eyre::eyre!("Stopped")),
            }
        }
        .await;
        if let Err(e) = &result {
            tracing::warn!("Session ended: {e}");
        }
        sessions().lock().unwrap().remove(&handle);
        let error = result.err().map(|e| e.to_string());
        platform.call_strings("ended", "(Ljava/lang/String;)V", &[error.as_deref()]);
    });
    handle
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_cfraz89_slipstream_Native_stopSession(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if let Some(ctl) = session(handle) {
        ctl.cancel.cancel();
    }
}

/// `iface` null means the join failed, with `detail` saying why; otherwise `detail` is the MAC
/// the interface joined with.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_cfraz89_slipstream_Native_wifiJoined(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    iface: JString,
    detail: JString,
) {
    let Some(ctl) = session(handle) else { return };
    let detail = string(&mut env, &detail).unwrap_or_default();
    let result = match string(&mut env, &iface) {
        Some(iface) => Ok(JoinedWifi { iface, mac: detail }),
        None => Err(detail),
    };
    ctl.wifi_joined(result);
}

/// One encoder output buffer. Blocks while the accessory is behind.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_cfraz89_slipstream_Native_pushFrame(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    buffer: JByteBuffer,
    offset: jint,
    size: jint,
    keyframe: jboolean,
    codec_config: jboolean,
) {
    let Some(ctl) = session(handle) else { return };
    let (Ok(ptr), Ok(capacity)) = (
        env.get_direct_buffer_address(&buffer),
        env.get_direct_buffer_capacity(&buffer),
    ) else {
        return;
    };
    let (offset, size) = (offset as usize, size as usize);
    if offset + size > capacity {
        return;
    }
    // SAFETY: a direct buffer of `capacity` bytes that the encoder keeps until this returns.
    let data = unsafe { std::slice::from_raw_parts(ptr.add(offset), size) };
    ctl.push_frame(data, keyframe != 0, codec_config != 0);
}
