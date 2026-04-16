use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::Stream;

use reqwest::RequestBuilder;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::{Client, Params, Result};

#[cfg(not(target_arch = "wasm32"))]
use futures_util::future::BoxFuture;

// on wasm32 our futures cannot be `Send`.
#[cfg(target_arch = "wasm32")]
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

pub type ResponseFuture<R> = BoxFuture<'static, crate::Result<MaybeContinue<R>>>;

#[derive(Deserialize, Debug)]
pub struct MaybeContinue<T> {
    #[serde(rename = "continue", default)]
    pub cont: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub inner: T,
}

#[derive(Default)]
#[pin_project::pin_project(project = StateProj)]
pub enum State<R, I> {
    #[default]
    Init,
    Fut(#[pin] ResponseFuture<R>),
    Values(I, Option<serde_json::Map<String, Value>>),
    Cont(serde_json::Map<String, Value>),
    Done,
}

impl<R, I: ExactSizeIterator> State<R, I> {
    pub fn values(v: I, cont: Option<serde_json::Map<String, Value>>) -> Self {
        if v.len() == 0 {
            if let Some(c) = cont {
                Self::Cont(c)
            } else {
                Self::Done
            }
        } else {
            Self::Values(v, cont)
        }
    }
}

#[pin_project::pin_project]
pub struct ContinueStream<P: Params + Clone, R, I: IntoIterator, F: Fn(R) -> Result<I>> {
    client: crate::Client,
    params: P,
    #[pin]
    state: State<R, I::IntoIter>,
    map: F,
}

impl<
    P: Params + Clone + Send,
    R: DeserializeOwned + 'static,
    I: IntoIterator,
    F: Fn(R) -> Result<I>,
> Stream for ContinueStream<P, R, I, F>
where
    I::IntoIter: ExactSizeIterator + Default,
{
    type Item = crate::Result<I::Item>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.as_mut().project();
        macro_rules! tryit {
            ($e:expr) => {
                match $e {
                    Ok(very_well) => very_well,
                    Err(e) => {
                        this.state.set(State::Done);
                        return Poll::Ready(Some(Err(e.into())));
                    }
                }
            };
        }

        let extra = match this.state.as_mut().project() {
            StateProj::Init => None,
            StateProj::Cont(v) => Some(std::mem::take(v)),
            StateProj::Values(v, cont) => {
                let value = v.next().expect("must always have value");
                let state = State::values(std::mem::take(v), std::mem::take(cont));
                this.state.set(state);
                return Poll::Ready(Some(Ok(value)));
            }
            StateProj::Fut(f) => match f.poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(res) => {
                    let res = tryit!(res);
                    let items = tryit!((this.map)(res.inner));
                    let mut iter = items.into_iter();
                    if let Some(item) = iter.next() {
                        this.state.set(State::values(iter, res.cont));
                        return Poll::Ready(Some(Ok(item)));
                    } else {
                        assert!(res.cont.is_none(), "Cannot continue without return value");
                        return Poll::Ready(None);
                    }
                }
            },
            StateProj::Done => return Poll::Ready(None),
        };

        pub struct Adhoc<P> {
            params: P,
            extra: Option<Map<String, Value>>,
        }

        impl<P: Params> Params for Adhoc<P> {
            fn len(&self) -> usize {
                self.params.len() + self.extra.as_ref().map_or(0, |v| v.len())
            }
            fn serialize_into<S: serde::ser::SerializeSeq>(
                &self,
                seq: &mut S,
            ) -> Result<(), S::Error> {
                self.params.serialize_into(seq)?;
                if let Some(map) = &self.extra {
                    for pair in map {
                        seq.serialize_element(&pair)?;
                    }
                }

                Ok(())
            }
        }

        async fn send_and_parse<R: DeserializeOwned>(
            r: RequestBuilder,
        ) -> crate::Result<MaybeContinue<R>> {
            Ok(r.send().await?.json().await?)
        }

        let params = this.params.clone();

        let req = this.client.get(Adhoc { params, extra });

        this.state.set(State::Fut(Box::pin(send_and_parse(req))));

        self.poll_next(cx)
    }
}

impl Client {
    pub fn get_all<R: DeserializeOwned + 'static, I: IntoIterator>(
        &self,
        params: impl Params + Clone + Send,
        map: impl Fn(R) -> Result<I>,
    ) -> impl Stream<Item = Result<I::Item>>
    where
        I::IntoIter: ExactSizeIterator + Default,
    {
        ContinueStream {
            client: self.clone(),
            params,
            state: State::Init,
            map,
        }
    }
}
