//! Raw V4L2 and v4l2loopback ABI: just the structs and ioctls the virtual
//! camera needs. Layouts follow <linux/videodev2.h> and v4l2loopback.h on
//! 64-bit Linux; the size asserts at the bottom catch a wrong layout at
//! compile time.

#![allow(non_camel_case_types)]

use std::mem::size_of;

pub const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
pub const V4L2_FIELD_NONE: u32 = 1;
pub const V4L2_COLORSPACE_REC709: u32 = 3;
pub const V4L2_PIX_FMT_YUYV: u32 = fourcc(b"YUYV");

const V4L2_EVENT_PRIVATE_START: u32 = 0x0800_0000;
/// v4l2loopback (0.13+) queues this when a reader starts or stops streaming;
/// its payload is a u32 that is 1 while a reader streams and 0 otherwise.
pub const V4L2_EVENT_PRI_CLIENT_USAGE: u32 = V4L2_EVENT_PRIVATE_START + 0x08E0_0000 + 1;

/// The first v4l2loopback release with the client-usage event.
pub const MIN_LOOPBACK_VERSION: (u32, u32, u32) = (0, 13, 0);

pub const fn fourcc(code: &[u8; 4]) -> u32 {
    (code[0] as u32) | (code[1] as u32) << 8 | (code[2] as u32) << 16 | (code[3] as u32) << 24
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_capability {
    pub driver: [u8; 16],
    pub card: [u8; 32],
    pub bus_info: [u8; 32],
    pub version: u32,
    pub capabilities: u32,
    pub device_caps: u32,
    pub reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_pix_format {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub bytesperline: u32,
    pub sizeimage: u32,
    pub colorspace: u32,
    pub priv_: u32,
    pub flags: u32,
    pub ycbcr_enc: u32,
    pub quantization: u32,
    pub xfer_func: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union v4l2_format_union {
    pub pix: v4l2_pix_format,
    pub raw_data: [u8; 200],
    // The kernel union holds pointers (struct v4l2_window), so it's 8-aligned.
    _align: [u64; 25],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_format {
    pub type_: u32,
    pub fmt: v4l2_format_union,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_fract {
    pub numerator: u32,
    pub denominator: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_outputparm {
    pub capability: u32,
    pub outputmode: u32,
    pub timeperframe: v4l2_fract,
    pub extendedmode: u32,
    pub writebuffers: u32,
    pub reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union v4l2_streamparm_union {
    pub output: v4l2_outputparm,
    pub raw_data: [u8; 200],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_streamparm {
    pub type_: u32,
    pub parm: v4l2_streamparm_union,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_event_subscription {
    pub type_: u32,
    pub id: u32,
    pub flags: u32,
    pub reserved: [u32; 5],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_event {
    pub type_: u32,
    // The kernel union (64 bytes) holds an s64, so it's 8-aligned.
    pub u: [u64; 8],
    pub pending: u32,
    pub sequence: u32,
    pub timestamp: libc::timespec,
    pub id: u32,
    pub reserved: [u32; 8],
}

/// struct v4l2_loopback_config from v4l2loopback.h.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_loopback_config {
    pub output_nr: i32,
    pub unused: i32,
    pub card_label: [u8; 32],
    pub min_width: u32,
    pub max_width: u32,
    pub min_height: u32,
    pub max_height: u32,
    pub max_buffers: i32,
    pub max_openers: i32,
    pub debug: i32,
    pub announce_all_caps: i32,
}

nix::ioctl_read!(vidioc_querycap, b'V', 0, v4l2_capability);
nix::ioctl_readwrite!(vidioc_s_fmt, b'V', 5, v4l2_format);
nix::ioctl_readwrite!(vidioc_s_parm, b'V', 22, v4l2_streamparm);
nix::ioctl_read!(vidioc_dqevent, b'V', 89, v4l2_event);
nix::ioctl_write_ptr!(vidioc_subscribe_event, b'V', 90, v4l2_event_subscription);

nix::ioctl_read!(loopback_ctl_version, b'~', 0, u32);
// Returns the new device's number.
nix::ioctl_write_ptr!(loopback_ctl_add, b'~', 1, v4l2_loopback_config);

const _: () = {
    assert!(size_of::<v4l2_capability>() == 104);
    assert!(size_of::<v4l2_pix_format>() == 48);
    assert!(size_of::<v4l2_format>() == 208);
    assert!(size_of::<v4l2_streamparm>() == 204);
    assert!(size_of::<v4l2_event_subscription>() == 32);
    assert!(size_of::<v4l2_event>() == 136);
    assert!(size_of::<v4l2_loopback_config>() == 72);
};
