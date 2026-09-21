// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    output::{Output, WeakOutput},
    reexports::{
        wayland_protocols_wlr::gamma_control::v1::server::{
            zwlr_gamma_control_manager_v1::{self, ZwlrGammaControlManagerV1},
            zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::GlobalId,
        },
    },
};
use std::{collections::HashMap, io, os::unix::io::OwnedFd};
use wayland_backend::server::ClientId;

/// Gamma ramps for the red, green and blue channels of an output.
pub type GammaRamps = (Vec<u16>, Vec<u16>, Vec<u16>);

pub trait GammaControlHandler {
    fn gamma_control_state(&mut self) -> &mut GammaControlState;
    fn gamma_size(&mut self, output: &Output) -> Option<usize>;
    /// Apply gamma ramps to the output, or restore the original gamma tables if `ramps` is `None`.
    fn set_gamma(&mut self, output: &Output, ramps: Option<GammaRamps>);
}

#[derive(Debug)]
pub struct GammaControlState {
    global: GlobalId,
    gamma_controls: HashMap<ZwlrGammaControlV1, GammaControl>,
}

impl GammaControlState {
    pub fn new<D, F>(dh: &DisplayHandle, client_filter: F) -> GammaControlState
    where
        D: GlobalDispatch<ZwlrGammaControlManagerV1, GammaControlManagerGlobalData> + 'static,
        F: for<'a> Fn(&'a Client) -> bool + Clone + Send + Sync + 'static,
    {
        let global = dh.create_global::<D, ZwlrGammaControlManagerV1, _>(
            1,
            GammaControlManagerGlobalData {
                filter: Box::new(client_filter.clone()),
            },
        );

        GammaControlState {
            global,
            gamma_controls: HashMap::new(),
        }
    }

    pub fn global_id(&self) -> GlobalId {
        self.global.clone()
    }
}

#[derive(Debug)]
struct GammaControl {
    /// `Some(size)` while the control object is valid, `None` after `failed` was sent.
    gamma_size: Option<usize>,
}

pub struct GammaControlManagerGlobalData {
    filter: Box<dyn for<'a> Fn(&'a Client) -> bool + Send + Sync>,
}

pub struct GammaControlData {
    output: WeakOutput,
}

impl<D> GlobalDispatch<ZwlrGammaControlManagerV1, GammaControlManagerGlobalData, D>
    for GammaControlState
where
    D: GlobalDispatch<ZwlrGammaControlManagerV1, GammaControlManagerGlobalData>
        + Dispatch<ZwlrGammaControlManagerV1, ()>
        + 'static,
{
    fn bind(
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrGammaControlManagerV1>,
        _global_data: &GammaControlManagerGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &GammaControlManagerGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<ZwlrGammaControlManagerV1, (), D> for GammaControlState
where
    D: GlobalDispatch<ZwlrGammaControlManagerV1, GammaControlManagerGlobalData>
        + Dispatch<ZwlrGammaControlManagerV1, ()>
        + Dispatch<ZwlrGammaControlV1, GammaControlData>
        + GammaControlHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _obj: &ZwlrGammaControlManagerV1,
        request: zwlr_gamma_control_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwlr_gamma_control_manager_v1::Request::GetGammaControl { id, output } => {
                let output = Output::from_resource(&output);

                // There can only be one gamma control object per output.
                let already_taken = output.as_ref().is_some_and(|output| {
                    state.gamma_control_state().gamma_controls.iter().any(
                        |(control, gamma_control)| {
                            gamma_control.gamma_size.is_some()
                                && control
                                    .data::<GammaControlData>()
                                    .and_then(|data| data.output.upgrade())
                                    .as_ref()
                                    .map(|control_output| control_output == output)
                                    .unwrap_or(false)
                        },
                    )
                });
                let gamma_size = output.as_ref().and_then(|output| state.gamma_size(output));

                let control = data_init.init(
                    id,
                    GammaControlData {
                        output: output.as_ref().map(|output| output.downgrade()).unwrap_or_default(),
                    },
                );

                if already_taken {
                    control.failed();
                    state
                        .gamma_control_state()
                        .gamma_controls
                        .insert(control, GammaControl { gamma_size: None });
                } else if let Some(size) = gamma_size {
                    control.gamma_size(size as u32);
                    state
                        .gamma_control_state()
                        .gamma_controls
                        .insert(control, GammaControl { gamma_size: Some(size) });
                } else {
                    control.failed();
                    state
                        .gamma_control_state()
                        .gamma_controls
                        .insert(control, GammaControl { gamma_size: None });
                }
            }
            zwlr_gamma_control_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<ZwlrGammaControlV1, GammaControlData, D> for GammaControlState
where
    D: Dispatch<ZwlrGammaControlV1, GammaControlData> + GammaControlHandler + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        obj: &ZwlrGammaControlV1,
        request: zwlr_gamma_control_v1::Request,
        data: &GammaControlData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwlr_gamma_control_v1::Request::SetGamma { fd } => {
                let gamma_size = state
                    .gamma_control_state()
                    .gamma_controls
                    .get(obj)
                    .and_then(|gamma_control| gamma_control.gamma_size);

                let Some(size) = gamma_size else {
                    // The control object was already invalidated with a `failed` event.
                    return;
                };

                match read_gamma_ramps(fd, size) {
                    Ok(ramps) => {
                        if let Some(output) = data.output.upgrade() {
                            state.set_gamma(&output, Some(ramps));
                        } else {
                            obj.failed();
                            invalidate_control(state, obj);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(?err, "Failed to read gamma ramps from fd");
                        obj.failed();
                        invalidate_control(state, obj);
                    }
                }
            }
            zwlr_gamma_control_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut D,
        _client: ClientId,
        obj: &ZwlrGammaControlV1,
        data: &GammaControlData,
    ) {
        // If the control object is still valid, destroying it restores the
        // original gamma tables of the output.
        let was_valid = state
            .gamma_control_state()
            .gamma_controls
            .remove(obj)
            .map(|gamma_control| gamma_control.gamma_size.is_some())
            .unwrap_or(false);

        if was_valid
            && let Some(output) = data.output.upgrade()
        {
            state.set_gamma(&output, None);
        }
    }
}

fn invalidate_control<D: GammaControlHandler>(state: &mut D, obj: &ZwlrGammaControlV1) {
    if let Some(gamma_control) = state.gamma_control_state().gamma_controls.get_mut(obj) {
        gamma_control.gamma_size = None;
    }
}

/// Read three gamma ramps (red, green, blue) of `size` u16 values each from a file descriptor.
///
/// The ramps are encoded as `3 * size` native-endian u16 values, like wlroots does it.
fn read_gamma_ramps(fd: OwnedFd, size: usize) -> io::Result<GammaRamps> {
    use std::io::Read;

    let mut file = io::BufReader::new(std::fs::File::from(fd));
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    if buf.len() != size * 3 * 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "gamma table has wrong size: expected {} bytes, got {}",
                size * 3 * 2,
                buf.len()
            ),
        ));
    }

    let ramps: Vec<u16> = buf
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| u16::from_ne_bytes(*chunk))
        .collect();
    let (red, rest) = ramps.split_at(size);
    let (green, blue) = rest.split_at(size);

    Ok((red.to_vec(), green.to_vec(), blue.to_vec()))
}

macro_rules! delegate_gamma_control {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt:tt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1: $crate::wayland::protocols::gamma_control::GammaControlManagerGlobalData
        ] => $crate::wayland::protocols::gamma_control::GammaControlState);
        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt:tt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1: ()
        ] => $crate::wayland::protocols::gamma_control::GammaControlState);
        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt:tt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_v1::ZwlrGammaControlV1: $crate::wayland::protocols::gamma_control::GammaControlData
        ] => $crate::wayland::protocols::gamma_control::GammaControlState);
    };
}
pub(crate) use delegate_gamma_control;
