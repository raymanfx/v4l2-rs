use std::{io, mem, slice, sync::Arc};

use crate::buffer::{Arena as ArenaTrait, Stream as StreamTrait};
use crate::buffer::{Buffer, Metadata};
use crate::device;
use crate::io::userptr::arena::Arena;
use crate::memory::Memory;
use crate::v4l2;
use crate::v4l_sys::*;

/// Stream of user buffers
///
/// An arena instance is used internally for buffer handling.
pub struct Stream {
    handle: Arc<device::Handle>,
    arena: Arena,
    arena_index: usize,

    active: bool,
    queued: bool,
}

impl Stream {
    /// Returns a stream for frame capturing
    ///
    /// # Arguments
    ///
    /// * `dev` - Capture device ref to get its file descriptor
    ///
    /// # Example
    ///
    /// ```
    /// use v4l::capture::Device;
    /// use v4l::io::userptr::Stream;
    ///
    /// let dev = Device::new(0);
    /// if let Ok(dev) = dev {
    ///     let stream = Stream::new(&dev);
    /// }
    /// ```
    pub fn new(dev: &dyn device::Device) -> io::Result<Self> {
        Stream::with_buffers(dev, 4)
    }

    pub fn with_buffers(dev: &dyn device::Device, count: u32) -> io::Result<Self> {
        let mut arena = Arena::new(dev);
        arena.allocate(count)?;

        Ok(Stream {
            handle: dev.handle(),
            arena,
            arena_index: 0,
            active: false,
            // the arena queues up all buffers once during allocation
            queued: true,
        })
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop().unwrap();
    }
}

impl<'a> StreamTrait<'a> for Stream {
    type Item = Buffer<'a>;

    fn start(&mut self) -> io::Result<()> {
        unsafe {
            let mut typ = v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_CAPTURE;
            v4l2::ioctl(
                self.handle.fd(),
                v4l2::vidioc::VIDIOC_STREAMON,
                &mut typ as *mut _ as *mut std::os::raw::c_void,
            )?;
        }

        Ok(())
    }

    fn stop(&mut self) -> io::Result<()> {
        unsafe {
            let mut typ = v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_CAPTURE;
            v4l2::ioctl(
                self.handle.fd(),
                v4l2::vidioc::VIDIOC_STREAMOFF,
                &mut typ as *mut _ as *mut std::os::raw::c_void,
            )?;
        }

        Ok(())
    }

    fn queue(&mut self) -> io::Result<()> {
        if self.queued {
            return Ok(());
        }

        let mut v4l2_buf: v4l2_buffer;
        let buf = &mut self.arena.buffers()[self.arena_index as usize];
        unsafe {
            v4l2_buf = mem::zeroed();
            v4l2_buf.type_ = v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_CAPTURE;
            v4l2_buf.memory = Memory::UserPtr as u32;
            v4l2_buf.index = self.arena_index as u32;
            v4l2_buf.m.userptr = buf.as_ptr() as u64;
            v4l2_buf.length = buf.len() as u32;
            v4l2::ioctl(
                self.handle.fd(),
                v4l2::vidioc::VIDIOC_QBUF,
                &mut v4l2_buf as *mut _ as *mut std::os::raw::c_void,
            )?;
        }

        self.arena_index += 1;
        if self.arena_index == self.arena.buffers().len() {
            self.arena_index = 0;
        }

        Ok(())
    }

    fn dequeue(&mut self) -> io::Result<Self::Item> {
        let mut v4l2_buf: v4l2_buffer;
        unsafe {
            v4l2_buf = mem::zeroed();
            v4l2_buf.type_ = v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_CAPTURE;
            v4l2_buf.memory = Memory::UserPtr as u32;
            v4l2::ioctl(
                self.handle.fd(),
                v4l2::vidioc::VIDIOC_DQBUF,
                &mut v4l2_buf as *mut _ as *mut std::os::raw::c_void,
            )?;
        }
        self.queued = false;

        let buffers = self.arena.buffers();
        let mut index: Option<usize> = None;
        for i in 0..buffers.len() {
            let buf = &buffers[i];
            unsafe {
                if (buf.as_ptr()) == (v4l2_buf.m.userptr as *const u8) {
                    index = Some(i);
                    break;
                }
            }
        }

        if index.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "failed to find buffer",
            ));
        }

        // The borrow checker prevents us from handing out slices to the internal buffer pool
        // (self.bufs), so we work around this limitation by passing slices to the v4l2_buf
        // instance instead, which holds a pointer itself.
        // That pointer just points back to one of the buffers we allocated ourselves (self.bufs),
        // which we ensured by checking for the index earlier.

        let ptr;
        let view;
        unsafe {
            ptr = v4l2_buf.m.userptr as *mut u8;
            view = slice::from_raw_parts::<u8>(ptr, v4l2_buf.bytesused as usize);
        }

        let buf = Buffer::new(
            view,
            Metadata::new(
                v4l2_buf.sequence,
                v4l2_buf.timestamp.into(),
                v4l2_buf.flags.into(),
            ),
        );

        Ok(buf)
    }

    fn next(&mut self) -> io::Result<Self::Item> {
        if !self.active {
            self.start()?;
        }

        self.queue()?;
        self.dequeue()
    }
}