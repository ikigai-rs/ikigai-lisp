//! The bridge from the engine test's core 0.3 kernel to this crate's core 0.4 doors.
//!
//! `ikigai-engine` is tested here on its core 0.3 release (it moves to core 0.4 in wave 4 of
//! the core 0.4.0 cascade, epic #3893), so `tests/pipeline.rs` has two cores. The engine runs
//! in a core 0.3 kernel, and every exact binding of the core 0.4 space it is given is bound
//! there too, forwarding to a real core 0.4 kernel over that space. What crosses is the request
//! (verb, target and inline arguments; the old kernel has already read any reference the
//! endpoint's contract declares), each endpoint's [`Description`](ikigai_core::Description) as
//! the JSON both cores share, and the answer's bytes, media type and expiry. A caller's
//! capability does not cross: every request is issued as root on the core 0.4 side, as the
//! engine issued it on the old one, and this crate's access rules are pinned by the tests on
//! core 0.4 (`tests/conformance.rs`, the unit tests). A program's own requests (`(source …)`)
//! stay inside the core 0.4 kernel, which holds the whole space. See the manifest for when
//! this goes.

use std::collections::HashSet;
use std::sync::Arc;

use ikigai_core_for_old_modules as old;

/// Renders a core 0.4 description as the JSON the old kernel reads back.
struct NewJsonRenderer;

impl ikigai_core::MetaRenderer for NewJsonRenderer {
    fn render(
        &self,
        description: &ikigai_core::Description,
        _target: &ikigai_core::ReprType,
    ) -> ikigai_core::Result<ikigai_core::Representation> {
        Ok(ikigai_core::Representation::new(
            ikigai_core::ReprType::new("application/json"),
            serde_json::to_vec(description).expect("serialize description"),
        ))
    }
}

/// Every exact binding of `new_space` (the first, where members of a fallback overlap), under
/// the same pattern, endpoint name and description, each forwarding to one core 0.4 kernel
/// over `new_space`.
pub fn bridged<S: ikigai_core::Space + 'static>(new_space: S) -> old::EndpointSpace {
    let entries = new_space
        .entries()
        .expect("the bridged space enumerates its bindings");
    let new = Arc::new(ikigai_core::Kernel::with_meta_renderer(
        Arc::new(new_space),
        Arc::new(NewJsonRenderer),
    ));
    let mut seen = HashSet::new();
    let mut space = old::EndpointSpace::new();
    for entry in entries {
        if entry.pattern.contains('{') || !seen.insert(entry.pattern.clone()) {
            continue;
        }
        let target = ikigai_core::Iri::parse(entry.pattern.as_str()).expect("an exact pattern");
        let meta = futures::executor::block_on(new.issue(
            ikigai_core::Request::new(ikigai_core::Verb::Meta, target).with_arg(
                "as",
                ikigai_core::ArgRef::Inline(b"application/json".to_vec()),
            ),
            &ikigai_core::Capability::root(),
        ))
        .expect("every bridged binding describes itself");
        let description: old::Description =
            serde_json::from_slice(&meta.bytes).expect("a description both cores read");
        space = space.bind(
            old::Exact::new(entry.pattern.clone()),
            Bridge {
                kernel: new.clone(),
                name: entry.endpoint,
                description,
            },
        );
    }
    space
}

struct Bridge {
    kernel: Arc<ikigai_core::Kernel>,
    name: String,
    description: old::Description,
}

/// The request as core 0.4 spells it: same verb and target, inline arguments.
fn request(inv: &old::Invocation<'_>) -> old::Result<ikigai_core::Request> {
    let verb: ikigai_core::Verb =
        serde_json::from_value(serde_json::to_value(inv.request.verb).expect("serialize verb"))
            .map_err(|e| old::Error::Endpoint(format!("a verb core 0.4 lacks: {e}")))?;
    let target = ikigai_core::Iri::parse(inv.request.target.as_str())
        .map_err(|e| old::Error::Endpoint(e.to_string()))?;
    let mut request = ikigai_core::Request::new(verb, target);
    for (name, arg) in &inv.request.args {
        let old::ArgRef::Inline(value) = arg else {
            return Err(old::Error::Endpoint(format!(
                "the bridge only carries inline arguments, not `{name}`"
            )));
        };
        request = request.with_arg(name, ikigai_core::ArgRef::Inline(value.clone()));
    }
    Ok(request)
}

/// A core 0.4 error as core 0.3 spells it; a denial keeps its message.
fn error(new: ikigai_core::Error) -> old::Error {
    match new {
        ikigai_core::Error::NotFound(detail) => old::Error::NotFound(detail),
        ikigai_core::Error::Denied(denial) => old::Error::Denied(denial.message().to_string()),
        ikigai_core::Error::InvalidArgument { name, detail } => {
            old::Error::InvalidArgument { name, detail }
        }
        ikigai_core::Error::MissingArgument(name) => old::Error::MissingArgument(name),
        other => old::Error::Endpoint(other.to_string()),
    }
}

#[async_trait::async_trait]
impl old::Endpoint for Bridge {
    async fn invoke(&self, inv: &old::Invocation<'_>) -> old::Result<old::Representation> {
        let answer = self
            .kernel
            .issue(request(inv)?, &ikigai_core::Capability::root())
            .await
            .map_err(error)?;
        let mut repr_type = old::ReprType::new(answer.repr_type.media_type.as_str());
        for (key, value) in &answer.repr_type.params {
            repr_type = repr_type.with_param(key, value);
        }
        let mut out = old::Representation::new(repr_type, answer.bytes);
        out.expiry =
            serde_json::from_value(serde_json::to_value(answer.expiry).expect("serialize expiry"))
                .expect("an expiry both cores read");
        Ok(out)
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn describe(&self) -> old::Description {
        self.description.clone()
    }
}
