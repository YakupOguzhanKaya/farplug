use std::marker::PhantomData;

use bytes::{Bytes, BytesMut};
use serde::{de::DeserializeOwned, Serialize};
use tokio_util::codec::{Decoder, Encoder, LengthDelimitedCodec};

/// Frames larger than this are a protocol error rather than an allocation.
pub const MAX_FRAME: usize = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed frame: {0}")]
    Codec(#[from] postcard::Error),
}

/// Length-delimited postcard frames. `In` is what we decode, `Out` what we send,
/// so the two endpoints instantiate it with the types swapped.
pub struct FrameCodec<In, Out> {
    inner: LengthDelimitedCodec,
    _marker: PhantomData<(In, Out)>,
}

impl<In, Out> FrameCodec<In, Out> {
    pub fn new() -> Self {
        let inner = LengthDelimitedCodec::builder()
            .length_field_type::<u32>()
            .max_frame_length(MAX_FRAME)
            .new_codec();
        Self {
            inner,
            _marker: PhantomData,
        }
    }
}

impl<In, Out> Default for FrameCodec<In, Out> {
    fn default() -> Self {
        Self::new()
    }
}

impl<In: DeserializeOwned, Out> Decoder for FrameCodec<In, Out> {
    type Item = In;
    type Error = ProtoError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<In>, ProtoError> {
        match self.inner.decode(src)? {
            Some(bytes) => Ok(Some(postcard::from_bytes(&bytes)?)),
            None => Ok(None),
        }
    }
}

impl<In, Out: Serialize> Encoder<Out> for FrameCodec<In, Out> {
    type Error = ProtoError;

    fn encode(&mut self, item: Out, dst: &mut BytesMut) -> Result<(), ProtoError> {
        let body = postcard::to_stdvec(&item)?;
        self.inner.encode(Bytes::from(body), dst)?;
        Ok(())
    }
}
