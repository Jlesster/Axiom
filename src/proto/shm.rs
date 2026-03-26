use crate::state::Axiom;
use std::os::unix::io::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use wayland_server::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_shm::{self, Format, WlShm},
    wl_shm_pool::{self, WlShmPool},
};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New};

pub struct ShmPoolData {
    pub fd: Arc<OwnedFd>, // Arc so ShmBufferData can share it
    pub size: i32,
}

pub struct ShmBufferData {
    pub pool_fd: Arc<OwnedFd>, // shared ref to the pool's fd
    pub offset: i32,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: Format,
}

impl GlobalDispatch<WlShm, ()> for Axiom {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        res: New<WlShm>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        let shm = di.init(res, ());
        shm.format(Format::Argb8888);
        shm.format(Format::Xrgb8888);
    }
}

impl Dispatch<WlShm, ()> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlShm,
        req: wl_shm::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            wl_shm::Request::CreatePool { id, fd, size } => {
                di.init(
                    id,
                    ShmPoolData {
                        fd: Arc::new(fd),
                        size,
                    },
                );
            }
            _ => {}
        }
    }
}

impl Dispatch<WlShmPool, ShmPoolData> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlShmPool,
        req: wl_shm_pool::Request,
        data: &ShmPoolData,
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            wl_shm_pool::Request::CreateBuffer {
                id,
                offset,
                width,
                height,
                stride,
                format,
            } => {
                di.init(
                    id,
                    ShmBufferData {
                        pool_fd: Arc::clone(&data.fd),
                        offset,
                        width,
                        height,
                        stride,
                        format: format.into_result().unwrap_or(Format::Argb8888),
                    },
                );
            }
            wl_shm_pool::Request::Resize { size: _ } => {}
            wl_shm_pool::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<WlBuffer, ShmBufferData> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlBuffer,
        req: wl_buffer::Request,
        _: &ShmBufferData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match req {
            wl_buffer::Request::Destroy => {}
            _ => {}
        }
    }
}
