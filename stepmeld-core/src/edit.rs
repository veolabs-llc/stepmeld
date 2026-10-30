//! Edits to a Workflow's own WorkflowDefinition: steps added and
//! removed, Bindings made, unmade, reordered and guarded, labels, the
//! values an entry starts from. Each is a Command like any other
//! (`engine::apply` hands them here), lands only if the whole
//! definition then holds together (`Library::check_workflow`), keeps
//! the Workflow's Step records in step with the definition, and leaves
//! a History entry that holds enough to undo it.
//!
//! A step is named by its path: `typeset` at the top, `publish/package`
//! inside the nested definition the step `publish` carries. An edit
//! inside a nested definition changes the parent's copy of it, and is
//! possible only until that step starts: from then on the child
//! Workflow is a document of its own, edited as one.

use crate::definition::{Binding, PortRef, StepEntry, WorkflowDefinition};
use crate::engine::{self, Command, DEFINITION_RULE};
use crate::history::Entry;
use crate::library::Library;
use crate::workflow::{Actor, Run, Stamp, Workflow};
use crate::Error;
use serde_json::json;

fn refused(words: String) -> Error {
    Error::Refused(words)
}

/// A step path split into the nested steps on the way and the step
/// itself.
fn split_path(path: &str) -> (Vec<&str>, &str) {
    let mut parts: Vec<&str> = path.split('/').collect();
    let leaf = parts.pop().unwrap_or("");
    (parts, leaf)
}

/// The definition at the end of a path of nested steps, in `def`.
fn descend<'a>(def: &'a mut WorkflowDefinition, outer: &[&str]) -> Result<&'a mut WorkflowDefinition, Error> {
    let mut at = def;
    for name in outer {
        let entry = at.steps.iter_mut().find(|e| &e.name == name).ok_or_else(|| refused(format!("no step {name:?}")))?;
        at = entry.definition.as_mut().ok_or_else(|| refused(format!("step {name:?} is not a nested WorkflowDefinition")))?;
    }
    Ok(at)
}

/// An edit inside a nested step's definition is refused once that step
/// has started: its child Workflow is then edited on its own.
fn not_started(wf: &Workflow, outer: &[&str]) -> Result<(), Error> {
    if let Some(first) = outer.first() {
        let s = wf.step(first)?;
        if let Some(run) = s.latest() {
            let child = run.handle.clone().unwrap_or_else(|| "its child Workflow".into());
            return Err(refused(format!("step {first:?} has started; edit {child} on its own")));
        }
    }
    Ok(())
}

/// A step name as a WorkflowDefinition allows it.
fn check_name(name: &str) -> Result<(), Error> {
    if name.is_empty() {
        return Err(refused("a step needs a name".into()));
    }
    if name.contains('.') || name.contains('/') || name.chars().any(char::is_whitespace) {
        return Err(refused(format!("{name:?}: a step's name has no dot, no slash and no space")));
    }
    Ok(())
}

fn step_of(port: &PortRef) -> Result<&str, Error> {
    port.split().map(|(s, _)| s).ok_or_else(|| refused(format!("{port} is not step.port")))
}

/// The Bindings into the same Input as `index`, in the order tried.
fn alternatives(def: &WorkflowDefinition, index: usize) -> Vec<usize> {
    match def.bindings.get(index).map(|b| &b.to) {
        Some(to) => (0..def.bindings.len()).filter(|&i| def.bindings[i].to == *to).collect(),
        None => Vec::new(),
    }
}

/// Every Step's Inputs told whether a Binding feeds them, and the
/// Values that follow: an Input that lost its last Binding lets go of
/// the Value it was fed; one that gained its first lets go of the Value
/// a person set.
fn rebind(wf: &mut Workflow) {
    let def = wf.workflow_definition.clone();
    for (name, step) in &mut wf.steps {
        for (input, i) in &mut step.inputs {
            let bound = def.bindings_into(name, input).next().is_some();
            if bound != i.bound {
                i.value = None;
            }
            i.bound = bound;
        }
    }
    wf.propagate(&def);
}

/// Apply a structural Command; `None` when the Command is not one. The
/// Workflow is changed only when Ok.
pub fn apply(lib: &Library, wf: &mut Workflow, cmd: &Command, by: &Actor, now: &str) -> Result<Option<Vec<Entry>>, Error> {
    let id = wf.id.clone();
    let entry = |kind: &str, step: &str| Entry::new(now, by, &id, kind).step(step);
    let stamp = |rule: &str, step: &str| Stamp {
        at: now.to_string(),
        by: Actor::policy(rule, step),
    };
    let mut def = wf.workflow_definition.clone();
    let out = match cmd {
        Command::AddStep { name, uses, label } => {
            let (outer, leaf) = split_path(name);
            check_name(leaf)?;
            not_started(wf, &outer)?;
            let at = descend(&mut def, &outer)?;
            if at.entry(leaf).is_some() {
                return Err(refused(format!("step {name:?} is already there")));
            }
            let mut e = StepEntry::new(leaf, uses.clone());
            e.label = label.clone();
            // a nested definition is carried whole
            if let Some(inner) = lib.workflow(uses) {
                e.definition = Some(lib.inlined(inner));
            }
            at.steps.push(e.clone());
            lib.check_workflow(&def)?;
            if outer.is_empty() {
                let record = engine::step_record(lib, &def, &e, now)?;
                wf.steps.insert(leaf.to_string(), record);
            }
            wf.workflow_definition = def;
            rebind(wf);
            vec![entry("step-added", name).detail(json!({"uses": uses, "label": label}))]
        }
        Command::RemoveStep { step } => {
            let (outer, leaf) = split_path(step);
            not_started(wf, &outer)?;
            if outer.is_empty() && wf.step(leaf)?.latest().is_some_and(Run::live) {
                return Err(refused(format!("step {step:?} is running; cancel it first")));
            }
            let at = descend(&mut def, &outer)?;
            let pos = at.steps.iter().position(|e| e.name == leaf).ok_or_else(|| refused(format!("no step {step:?}")))?;
            let removed = at.steps.remove(pos);
            let mut gone = Vec::new();
            let mut kept = Vec::new();
            for (i, b) in at.bindings.drain(..).enumerate() {
                if step_of(&b.from)? == leaf || step_of(&b.to)? == leaf {
                    gone.push((i, b));
                } else {
                    kept.push(b);
                }
            }
            at.bindings = kept;
            lib.check_workflow(&def)?;
            let record = if outer.is_empty() { wf.steps.remove(leaf) } else { None };
            wf.workflow_definition = def;
            rebind(wf);
            vec![entry("step-removed", step).detail(json!({"entry": removed, "bindings": gone, "record": record}))]
        }
        Command::Bind { from, to, guard } => {
            let (outer, fs) = split_path(step_of(from)?);
            let (outer_to, ts) = split_path(step_of(to)?);
            if outer != outer_to {
                return Err(refused(format!("{from} and {to} are not in the same WorkflowDefinition")));
            }
            if fs == ts {
                return Err(refused(format!("{from} -> {to}: a step cannot feed itself")));
            }
            not_started(wf, &outer)?;
            let inner = |p: &PortRef| -> Result<PortRef, Error> {
                let (_, port) = p.split().ok_or_else(|| refused(format!("{p} is not step.port")))?;
                let (_, leaf) = split_path(step_of(p)?);
                Ok(PortRef::new(leaf, port))
            };
            let (f, t) = (inner(from)?, inner(to)?);
            let at = descend(&mut def, &outer)?;
            if at.bindings.iter().any(|b| b.from == f && b.to == t) {
                return Err(refused(format!("{from} already feeds {to}")));
            }
            if outer.is_empty() {
                let (_, port) = t.split().unwrap();
                if let Some(i) = wf.step(ts)?.inputs.get(port) {
                    if !i.bound && i.value.is_some() {
                        return Err(refused(format!("{to} holds a value set by hand; clear it first")));
                    }
                }
            }
            at.bindings.push(Binding { from: f, to: t, guard: guard.clone() });
            let index = at.bindings.len() - 1;
            lib.check_workflow(&def)?;
            wf.workflow_definition = def;
            rebind(wf);
            vec![entry("bound", ts).detail(json!({"index": index, "from": from, "to": to, "guard": guard, "within": within_word(&outer)}))]
        }
        Command::Unbind { index, within } => {
            let outer = within_path(within);
            not_started(wf, &outer)?;
            let at = descend(&mut def, &outer)?;
            if *index >= at.bindings.len() {
                return Err(refused(format!("no binding {index}")));
            }
            let removed = at.bindings.remove(*index);
            lib.check_workflow(&def)?;
            wf.workflow_definition = def;
            rebind(wf);
            let to = step_of(&removed.to)?.to_string();
            vec![entry("unbound", &to).detail(json!({"index": index, "binding": removed, "within": within}))]
        }
        Command::MoveBinding { index, earlier, within } => {
            let outer = within_path(within);
            not_started(wf, &outer)?;
            let at = descend(&mut def, &outer)?;
            let alts = alternatives(at, *index);
            let pos = alts.iter().position(|i| i == index).ok_or_else(|| refused(format!("no binding {index}")))?;
            let other = if *earlier { pos.checked_sub(1).map(|p| alts[p]) } else { alts.get(pos + 1).copied() };
            let other = other.ok_or_else(|| refused(format!("binding {index} is already tried {}", if *earlier { "first" } else { "last" })))?;
            at.bindings.swap(*index, other);
            let to = step_of(&at.bindings[other].to)?.to_string();
            lib.check_workflow(&def)?;
            wf.workflow_definition = def;
            rebind(wf);
            vec![entry("binding-moved", &to).detail(json!({"from": index, "to": other, "within": within}))]
        }
        Command::SetGuard { index, guard, within } => {
            let outer = within_path(within);
            not_started(wf, &outer)?;
            let at = descend(&mut def, &outer)?;
            let b = at.bindings.get_mut(*index).ok_or_else(|| refused(format!("no binding {index}")))?;
            let before = std::mem::replace(&mut b.guard, guard.clone());
            let to = step_of(&b.to)?.to_string();
            lib.check_workflow(&def)?;
            wf.workflow_definition = def;
            rebind(wf);
            vec![entry("guard-set", &to).detail(json!({"index": index, "before": before, "after": guard, "within": within}))]
        }
        Command::Relabel { step, label } => {
            let (outer, leaf) = split_path(step);
            not_started(wf, &outer)?;
            let at = descend(&mut def, &outer)?;
            let e = at.steps.iter_mut().find(|e| e.name == leaf).ok_or_else(|| refused(format!("no step {step:?}")))?;
            let before = std::mem::replace(&mut e.label, label.clone());
            if outer.is_empty() {
                let verb_label = lib.verb_in(&def, leaf).and_then(|v| v.label);
                wf.step_mut(leaf)?.label = label.clone().or(verb_label);
            }
            wf.workflow_definition = def;
            vec![entry("relabeled", step).detail(json!({"before": before, "after": label}))]
        }
        Command::SetOverride { step, parameter, value } => {
            let (outer, leaf) = split_path(step);
            not_started(wf, &outer)?;
            let at = descend(&mut def, &outer)?;
            let e = at.steps.iter_mut().find(|e| e.name == leaf).ok_or_else(|| refused(format!("no step {step:?}")))?;
            let before = match value {
                Some(v) => e.parameters.insert(parameter.clone(), v.clone()),
                None => e.parameters.remove(parameter),
            };
            lib.check_workflow(&def)?;
            // a Step not chosen by a person follows the definition
            if outer.is_empty() {
                let verb = lib.verb_in(&def, leaf).ok_or_else(|| Error::Invalid(format!("{step}: its verb is not in the library")))?;
                let p = wf.step_mut(leaf)?.parameters.get_mut(parameter).ok_or_else(|| refused(format!("step {step:?} has no parameter {parameter:?}")))?;
                if !p.chosen.as_ref().is_some_and(|c| c.by.is_someone()) {
                    match value {
                        Some(v) => {
                            p.value = Some(v.clone());
                            p.chosen = Some(stamp(DEFINITION_RULE, leaf));
                        }
                        None => {
                            p.value = verb.parameters.iter().find(|d| &d.name == parameter).and_then(|d| d.default.clone());
                            p.chosen = None;
                        }
                    }
                }
            }
            wf.workflow_definition = def;
            vec![entry("override-set", step).detail(json!({"parameter": parameter, "before": before, "after": value}))]
        }
        Command::Saved { definition } => {
            let before = wf.from.replace(definition.clone());
            vec![Entry::new(now, by, &id, "saved").detail(json!({"definition": definition, "before": before}))]
        }
        _ => return Ok(None),
    };
    Ok(Some(out))
}

fn within_path(within: &Option<String>) -> Vec<&str> {
    within.as_deref().map(|w| w.split('/').collect()).unwrap_or_default()
}

fn within_word(outer: &[&str]) -> Option<String> {
    (!outer.is_empty()).then(|| outer.join("/"))
}

/// What a Workflow's own definition keeps when it is saved to a
/// Library: beyond its steps and Bindings, which always go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Keep {
    /// Parameter values a person chose on this Workflow's Steps become
    /// the entries' own values.
    pub values: bool,
    /// A Step's Policy that differs from its verb's becomes the entry's.
    pub policies: bool,
    /// A placement on a Performer by name stays; otherwise it is let go,
    /// and the locality, if any, is what the definition says.
    pub placements_by_name: bool,
}

/// This Workflow's definition as one to keep in a Library, under a name
/// and version of its own: its steps and Bindings as they stand, nested
/// definitions inline, Inputs never (they are the Workflow's), and of
/// the Workflow's tuning what `keep` says.
pub fn definition_of(wf: &Workflow, lib: &Library, name: &str, version: u32, label: Option<&str>, keep: &Keep) -> WorkflowDefinition {
    let mut def = wf.workflow_definition.clone();
    def.name = name.to_string();
    def.version = version;
    def.label = label.map(str::to_string);
    for e in &mut def.steps {
        let Some(step) = wf.steps.get(&e.name) else { continue };
        let verb = lib.verb(e);
        if keep.values {
            for (p, v) in &step.parameters {
                if v.chosen.as_ref().is_some_and(|c| c.by.is_someone()) {
                    match &v.value {
                        Some(value) => {
                            e.parameters.insert(p.clone(), value.clone());
                        }
                        None => {
                            e.parameters.remove(p);
                        }
                    }
                }
            }
        }
        if keep.policies {
            e.policy = Some(step.policy.clone());
        }
        if let Some(p) = &mut e.policy {
            if !keep.placements_by_name {
                p.placement.performer = None;
            }
            if verb.as_ref().is_some_and(|v| v.policy == *p) {
                e.policy = None;
            }
        }
    }
    def
}
