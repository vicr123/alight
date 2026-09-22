use async_channel::Receiver;
use smol::stream::StreamExt;

pub struct ProgressIndication<T, E> {
    inner_rb: Option<Receiver<ProgressIndicationPacket<T, E>>>,
}

pub(crate) enum ProgressIndicationPacket<T, E> {
    Complete,
    Data(T),
    Err(E),
}

impl<T, E> From<T> for ProgressIndicationPacket<T, E> {
    fn from(value: T) -> Self {
        ProgressIndicationPacket::Data(value)
    }
}

impl<T, E> ProgressIndication<T, E> {
    pub(crate) fn new(inner_rb: Receiver<ProgressIndicationPacket<T, E>>) -> Self {
        Self {
            inner_rb: Some(inner_rb),
        }
    }

    pub async fn next(&mut self) -> Option<Result<T, E>> {
        let Some(ref mut inner_rb) = self.inner_rb else {
            return None;
        };

        match inner_rb.recv().await {
            Err(_) | Ok(ProgressIndicationPacket::Complete) => {
                self.inner_rb.take();
                None
            }
            Ok(ProgressIndicationPacket::Data(data)) => Some(Ok(data)),
            Ok(ProgressIndicationPacket::Err(err)) => {
                self.inner_rb.take();
                Some(Err(err))
            }
        }
    }
}
