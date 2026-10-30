//! The Library: what can be done, by name and version. Running never
//! writes to it.

use crate::definition::{DoneBy, Guard, InputDefinition, OutputDefinition, ParameterDefinition, Ref, StepDefinition, StepEntry, WorkflowDefinition, DECISION};
use crate::Error;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default, Clone)]
pub struct Library {
    steps: BTreeMap<Ref, StepDefinition>,
    workflows: BTreeMap<Ref, WorkflowDefinition>,
}

impl Library {
    pub fn new() -> Library {
        Library::default()
    }

    /// Add a verb; it is checked first.
    pub fn add_step(&mut self, def: StepDefinition) -> Result<(), Error> {
        def.check()?;
        if self.workflows.contains_key(&def.reference()) {
            return Err(Error::Invalid(format!("StepDefinition {}: a WorkflowDefinition has that name and version, and they share one namespace", def.reference())));
        }
        self.steps.insert(def.reference(), def);
        Ok(())
    }

    /// Add a recipe; it is checked against the verbs already here.
    /// Verbs share one namespace: a recipe may not take a step's name.
    pub fn add_workflow(&mut self, def: WorkflowDefinition) -> Result<(), Error> {
        self.check_workflow(&def)?;
        if self.steps.contains_key(&def.reference()) {
            return Err(Error::Invalid(format!("WorkflowDefinition {}: a StepDefinition has that name and version, and they share one namespace", def.reference())));
        }
        self.workflows.insert(def.reference(), def);
        Ok(())
    }

    /// Everything at once, checked after it is all in, so recipes may
    /// come in any order.
    pub fn from_parts(steps: impl IntoIterator<Item = StepDefinition>, workflows: impl IntoIterator<Item = WorkflowDefinition>) -> Result<Library, Error> {
        let mut lib = Library::new();
        for s in steps {
            lib.add_step(s)?;
        }
        for w in workflows {
            if lib.steps.contains_key(&w.reference()) {
                return Err(Error::Invalid(format!("WorkflowDefinition {}: a StepDefinition has that name and version, and they share one namespace", w.reference())));
            }
            lib.workflows.insert(w.reference(), w);
        }
        for w in lib.workflows.values() {
            lib.check_workflow(w)?;
        }
        Ok(lib)
    }

    /// The WorkflowDefinitions among `workflows` that use `r` as one of
    /// their Steps.
    pub fn users_of(workflows: &[WorkflowDefinition], r: &Ref) -> Vec<Ref> {
        workflows.iter().filter(|w| w.steps.iter().any(|e| &e.step == r)).map(|w| w.reference()).collect()
    }

    /// The StepDefinitions the WorkflowDefinition `removed` uses that no
    /// other WorkflowDefinition does: what removing it leaves orphaned.
    pub fn orphaned_by(steps: &[StepDefinition], workflows: &[WorkflowDefinition], removed: &Ref) -> Vec<Ref> {
        let Some(gone) = workflows.iter().find(|w| &w.reference() == removed) else { return Vec::new() };
        let others: Vec<WorkflowDefinition> = workflows.iter().filter(|w| &w.reference() != removed).cloned().collect();
        let mut out: Vec<Ref> = steps.iter().map(|s| s.reference()).filter(|r| gone.steps.iter().any(|e| &e.step == r) && Self::users_of(&others, r).is_empty()).collect();
        out.dedup();
        out
    }

    /// A verb: a StepDefinition, or a WorkflowDefinition seen from
    /// outside (its derived face), so recipes nest.
    pub fn step(&self, r: &Ref) -> Option<StepDefinition> {
        self.steps.get(r).cloned().or_else(|| self.workflows.get(r).map(|w| self.face_of(w)))
    }

    pub fn workflow(&self, r: &Ref) -> Option<&WorkflowDefinition> {
        self.workflows.get(r)
    }

    /// The verb a step entry is an occurrence of: the face of the
    /// WorkflowDefinition it carries inline, else what the Library holds
    /// under its name and version.
    pub fn verb(&self, entry: &StepEntry) -> Option<StepDefinition> {
        match &entry.definition {
            Some(inner) => Some(self.face_of(inner)),
            None => self.step(&entry.step),
        }
    }

    /// `verb`, for the step of that name in a WorkflowDefinition.
    pub fn verb_in(&self, w: &WorkflowDefinition, step: &str) -> Option<StepDefinition> {
        w.entry(step).and_then(|e| self.verb(e))
    }

    /// The definition with every nested WorkflowDefinition it names by
    /// name and version alone copied in, all the way down: what a
    /// Workflow is made from, so it is whole on its own. One the
    /// Library does not hold is left as named, for `check_workflow` to
    /// refuse.
    pub fn inlined(&self, w: &WorkflowDefinition) -> WorkflowDefinition {
        let mut out = w.clone();
        for entry in &mut out.steps {
            if entry.definition.is_none() {
                if let Some(inner) = self.workflows.get(&entry.step) {
                    entry.definition = Some(inner.clone());
                }
            }
            if let Some(inner) = entry.definition.take() {
                entry.definition = Some(self.inlined(&inner));
            }
        }
        out
    }

    pub fn steps(&self) -> impl Iterator<Item = &StepDefinition> {
        self.steps.values()
    }

    pub fn workflows(&self) -> impl Iterator<Item = &WorkflowDefinition> {
        self.workflows.values()
    }

    /// A recipe's derived face: every required Input no Binding feeds
    /// (`step.input`), every Parameter (`step.parameter`), every Output
    /// (`step.output`). The Policy is the default one; placement is the
    /// nested Performer's business.
    pub fn face_of(&self, w: &WorkflowDefinition) -> StepDefinition {
        let mut face = StepDefinition::new(&w.name, w.version);
        face.label = w.label.clone();
        face.description = w.description.clone();
        for entry in &w.steps {
            let Some(def) = self.verb(entry) else { continue };
            for i in &def.inputs {
                if w.bindings_into(&entry.name, &i.name).next().is_none() {
                    face.inputs.push(InputDefinition {
                        name: format!("{}.{}", entry.name, i.name),
                        tag: i.tag.clone(),
                        required: i.required,
                        label: i.label.clone(),
                    });
                }
            }
            for p in &def.parameters {
                face.parameters.push(ParameterDefinition {
                    name: format!("{}.{}", entry.name, p.name),
                    tag: p.tag.clone(),
                    required: p.required,
                    default: p.default.clone(),
                    label: p.label.clone(),
                    group: p.group.clone(),
                });
            }
            for o in &def.outputs {
                face.outputs.push(OutputDefinition {
                    name: format!("{}.{}", entry.name, o.name),
                    tag: o.tag.clone(),
                    options: o.options.clone(),
                    label: o.label.clone(),
                });
            }
        }
        face
    }

    /// Names unique and undotted, every entry's verb known (a nested
    /// definition carried inline checked the same way, under the name
    /// and version the entry gives it), every Parameter value named one
    /// the verb has, every Binding between ports that exist with equal
    /// tags, every Guard over a step and Decision that exist, no
    /// cycles. A definition with no steps holds together: a Workflow
    /// made from it is empty until steps are added to it.
    pub fn check_workflow(&self, w: &WorkflowDefinition) -> Result<(), Error> {
        let invalid = |m: String| Error::Invalid(format!("WorkflowDefinition {}: {m}", w.reference()));
        if w.contract != crate::definition::WORKFLOW_DEFINITION {
            return Err(invalid(format!("contract is {:?}", w.contract)));
        }
        let mut defs: BTreeMap<&str, StepDefinition> = BTreeMap::new();
        for entry in &w.steps {
            let (name, step) = (&entry.name, &entry.step);
            if name.is_empty() || name.contains('.') {
                return Err(invalid(format!("step name {name:?} is empty or contains a dot")));
            }
            if defs.contains_key(name.as_str()) {
                return Err(invalid(format!("step name {name:?} is used twice")));
            }
            if step == &w.reference() {
                return Err(invalid(format!("step {name:?} is the recipe itself")));
            }
            let def = match &entry.definition {
                Some(inner) => {
                    if inner.reference() != *step {
                        return Err(invalid(format!("step {name:?} carries {} but names {step}", inner.reference())));
                    }
                    self.check_workflow(inner)?;
                    self.face_of(inner)
                }
                None => self.step(step).ok_or_else(|| invalid(format!("step {name:?} refers to {step}, which is not in the library")))?,
            };
            for p in entry.parameters.keys() {
                if !def.parameters.iter().any(|d| &d.name == p) {
                    return Err(invalid(format!("step {name:?} sets parameter {p:?}, which {step} does not have")));
                }
            }
            defs.insert(name, def);
        }
        fn port(r: &crate::definition::PortRef) -> Result<(&str, &str), String> {
            r.split().ok_or_else(|| format!("{r} is not step.port"))
        }
        for b in &w.bindings {
            let (fs, fp) = port(&b.from).map_err(invalid)?;
            let (ts, tp) = port(&b.to).map_err(invalid)?;
            let from = defs.get(fs).ok_or_else(|| invalid(format!("binding from {}: no step {fs:?}", b.from)))?;
            let to = defs.get(ts).ok_or_else(|| invalid(format!("binding to {}: no step {ts:?}", b.to)))?;
            let out = from.output(fp).ok_or_else(|| invalid(format!("binding from {}: {fs:?} has no output {fp:?}", b.from)))?;
            let inp = to.input(tp).ok_or_else(|| invalid(format!("binding to {}: {ts:?} has no input {tp:?}", b.to)))?;
            if out.tag != inp.tag {
                return Err(invalid(format!("binding {} -> {}: tags differ ({:?} vs {:?})", b.from, b.to, out.tag, inp.tag)));
            }
            if fs == ts {
                return Err(invalid(format!("binding {} -> {}: a step cannot feed itself", b.from, b.to)));
            }
            if let Some(g) = &b.guard {
                check_guard(g, &defs).map_err(|m| invalid(format!("binding {} -> {}: {m}", b.from, b.to)))?;
            }
        }
        // no cycles: every step's upstream closure excludes itself
        for entry in &w.steps {
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            let mut frontier: Vec<&str> = w.upstream(&entry.name).into_iter().collect();
            while let Some(s) = frontier.pop() {
                if s == entry.name {
                    return Err(invalid(format!("step {:?} depends on itself", entry.name)));
                }
                if seen.insert(s) {
                    frontier.extend(w.upstream(s));
                }
            }
        }
        for (name, def) in &defs {
            if def.done_by == DoneBy::Person && def.outputs.is_empty() && def.inputs.is_empty() {
                let _ = name; // a person's step with nothing to give or take is odd but allowed
            }
        }
        Ok(())
    }
}

fn check_guard(g: &Guard, defs: &BTreeMap<&str, StepDefinition>) -> Result<(), String> {
    match g {
        Guard::Decision { step, output, is } => {
            let def = defs.get(step.as_str()).ok_or_else(|| format!("guard names step {step:?}, which is not in the recipe"))?;
            let out = def.output(output).ok_or_else(|| format!("guard names {step}.{output}, which {step:?} does not produce"))?;
            match &out.options {
                Some(opts) if opts.contains(is) => Ok(()),
                Some(_) => Err(format!("guard asks {step}.{output} for {is:?}, which is not one of its options")),
                None => Err(format!("guard reads {step}.{output}, which is not a {DECISION}")),
            }
        }
        Guard::Status { step, is } => {
            defs.get(step.as_str()).ok_or_else(|| format!("guard names step {step:?}, which is not in the recipe"))?;
            if !["succeeded", "failed", "skipped"].contains(&is.as_str()) {
                return Err(format!("guard asks for status {is:?}; a guard may ask for succeeded, failed or skipped"));
            }
            Ok(())
        }
        Guard::Present { step, output } => {
            let def = defs.get(step.as_str()).ok_or_else(|| format!("guard names step {step:?}, which is not in the recipe"))?;
            def.output(output).map(|_| ()).ok_or_else(|| format!("guard names {step}.{output}, which {step:?} does not produce"))
        }
        Guard::All(gs) | Guard::Any(gs) => gs.iter().try_for_each(|g| check_guard(g, defs)),
        Guard::Not(g) => check_guard(g, defs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::StepEntry;

    fn step(name: &str) -> StepDefinition {
        let mut s = StepDefinition::new(name, 1);
        s.program = Some("p".into());
        s
    }

    fn workflow(name: &str, uses: &[&str]) -> WorkflowDefinition {
        let mut w = WorkflowDefinition::new(name, 1);
        w.steps = uses.iter().enumerate().map(|(i, u)| StepEntry::new(&format!("s{i}"), Ref::new(u, 1))).collect();
        w
    }

    /// Removing a WorkflowDefinition orphans the StepDefinitions only it
    /// used; one another WorkflowDefinition uses, or none used, is not
    /// its orphan.
    #[test]
    fn users_and_orphans() {
        let steps = vec![step("stepdef_add"), step("stepdef_multiply"), step("stepdef_shared"), step("stepdef_loose")];
        let workflows = vec![workflow("workflowdef_hello", &["stepdef_add", "stepdef_multiply", "stepdef_shared"]), workflow("workflowdef_other", &["stepdef_shared"])];
        assert_eq!(Library::users_of(&workflows, &Ref::new("stepdef_shared", 1)), vec![Ref::new("workflowdef_hello", 1), Ref::new("workflowdef_other", 1)]);
        assert!(Library::users_of(&workflows, &Ref::new("stepdef_loose", 1)).is_empty());
        assert_eq!(Library::orphaned_by(&steps, &workflows, &Ref::new("workflowdef_hello", 1)), vec![Ref::new("stepdef_add", 1), Ref::new("stepdef_multiply", 1)]);
        assert!(Library::orphaned_by(&steps, &workflows, &Ref::new("nope", 1)).is_empty());
    }
}
