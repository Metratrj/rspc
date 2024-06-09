use std::marker::PhantomData;

use actix_web::{web, HttpRequest};
use rspc::ExecError;

pub trait TCtxFunc<TCtx, TMarker>: Clone + Send + Sync + 'static + Unpin
where
    TCtx: Send + 'static,
{
    fn exec<'req>(
        &self,
        req: &HttpRequest,
        state: Option<&web::Data<TCtx>>,
    ) -> Result<TCtx, ExecError>
    where
        TCtx: Send + 'req;
}

pub struct NoArgMarker(PhantomData<()>);

impl<TCtx, TFunc> TCtxFunc<TCtx, NoArgMarker> for TFunc
where
    TFunc: Fn() -> TCtx + Clone + Send + Sync + 'static + Unpin,
    TCtx: Send + 'static,
{
    fn exec<'req>(
        &self,
        _req: &HttpRequest,
        _state: Option<&web::Data<TCtx>>,
    ) -> Result<TCtx, ExecError>
    where
        TCtx: Send + 'req,
    {
        Ok(self())
    }
}

pub struct SingleArgMarker(PhantomData<()>);

impl<TCtx, TFunc> TCtxFunc<TCtx, SingleArgMarker> for TFunc
where
    TFunc: Fn(&HttpRequest, Option<&web::Data<TCtx>>) -> TCtx + Clone + Send + Sync + 'static + Unpin,
    TCtx: Send + 'static,
{
    fn exec<'req>(
        &self,
        req: &HttpRequest,
        state: Option<&web::Data<TCtx>>,
    ) -> Result<TCtx, ExecError>
    where
        TCtx: Send + 'req,
    {
        Ok(self(req, state))
    }
}
