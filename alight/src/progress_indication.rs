use async_ringbuf::traits::RingBuffer;
use async_ringbuf::{AsyncHeapCons, AsyncRb};
use smol::stream::StreamExt;

pub struct ProgressIndication<T, E> {
    inner_rb: Option<AsyncHeapCons<ProgressIndicationPacket<T, E>>>,
}

pub(crate) enum ProgressIndicationPacket<T, E> {
    Complete,
    Data(T),
    Err(E),
}

impl<T, E> ProgressIndication<T, E> {
    pub(crate) fn new(inner_rb: AsyncHeapCons<ProgressIndicationPacket<T, E>>) -> Self {
        Self {
            inner_rb: Some(inner_rb),
        }
    }

    pub async fn next(&mut self) -> Option<Result<T, E>> {
        let Some(ref mut inner_rb) = self.inner_rb else {
            return None;
        };

        match inner_rb.next().await {
            None | Some(ProgressIndicationPacket::Complete) => {
                self.inner_rb.take();
                None
            }
            Some(ProgressIndicationPacket::Data(data)) => Some(Ok(data)),
            Some(ProgressIndicationPacket::Err(err)) => {
                self.inner_rb.take();
                Some(Err(err))
            }
        }
    }
}
