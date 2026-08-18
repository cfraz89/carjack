use uuid::Uuid;

pub const CARPLAY_DEVICE_UUID: Uuid = Uuid::from_u128(0x2D8D2466E14D451C88BC7301ABEA291A);
pub const CARPLAY_ACCESSORY_UUID: Uuid = Uuid::from_u128(0xEC884348CD4140A29727575D50BF1FD3);
pub const IAP_DEVICE_UUID: Uuid = Uuid::from_u128(0x00000000decafadedecadeafdecacafe);
pub const IAP_ACCESSORY_UUID: Uuid = Uuid::from_u128(0x00000000decafadedecadeafdecacaff);
pub const IAP2_UUID: Uuid = Uuid::from_u128(0x020303021d19415f86f222a2106a0a77);
pub const UNKNOWN_UUID: Uuid = Uuid::from_u128(0x4de17a0052cb11e6bdf40800200c9a66);

/// Assigned-number services in their full form: `xxxxxxxx-0000-1000-8000-00805F9B34FB`. BlueZ
/// folds these back to the 16-bit EIR list, which is where a unit looks for them.
const fn sig_uuid(short: u32) -> Uuid {
    Uuid::from_u128(((short as u128) << 96) | 0x0000_1000_8000_00805F9B34FB)
}

/// The hands-free gateway carjack serves itself, so the identity probe on its AT channel reaches
/// us rather than the host's modem-less one.
pub const HANDS_FREE_AG_UUID: Uuid = sig_uuid(0x111f);
