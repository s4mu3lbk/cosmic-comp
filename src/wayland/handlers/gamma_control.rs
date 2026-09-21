// SPDX-License-Identifier: GPL-3.0-only

use smithay::{output::Output, reexports::drm::control::Device as ControlDevice};

use crate::{
    backend::kms::Surface,
    state::{BackendData, State},
    utils::prelude::OutputExt,
    wayland::protocols::gamma_control::{
        GammaControlHandler, GammaControlState, GammaRamps, delegate_gamma_control,
    },
};

fn kms_surfaces(state: &mut State) -> impl Iterator<Item = &mut Surface> {
    if let BackendData::Kms(kms_state) = &mut state.backend {
        Some(
            kms_state
                .drm_devices
                .values_mut()
                .flat_map(|device| device.inner.surfaces.values_mut()),
        )
    } else {
        None
    }
    .into_iter()
    .flatten()
}

// Get KMS `Surface` for output, and for all outputs mirroring it
fn kms_surfaces_for_output<'a>(
    state: &'a mut State,
    output: &'a Output,
) -> impl Iterator<Item = &'a mut Surface> + 'a {
    kms_surfaces(state).filter(move |surface| {
        surface.output == *output || surface.output.mirroring().as_ref() == Some(output)
    })
}

impl GammaControlHandler for State {
    fn gamma_control_state(&mut self) -> &mut GammaControlState {
        &mut self.common.gamma_control_state
    }

    fn gamma_size(&mut self, output: &Output) -> Option<usize> {
        let BackendData::Kms(kms_state) = &self.backend else {
            return None;
        };

        for device in kms_state.drm_devices.values() {
            for surface in device.inner.surfaces.values() {
                if surface.output == *output {
                    let crtc = device.drm.device().get_crtc(surface.crtc()).ok()?;
                    return usize::try_from(crtc.gamma_length()).ok();
                }
            }
        }

        None
    }

    fn set_gamma(&mut self, output: &Output, ramps: Option<GammaRamps>) {
        for surface in kms_surfaces_for_output(self, output) {
            surface.set_gamma(ramps.clone());
        }
    }
}

delegate_gamma_control!(State);
