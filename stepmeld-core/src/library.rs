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
            return Err(Error::Invalid(format!("step definition {}: a workflow definition has that name and version; verbs share one namespace", def.reference())));
        }
        self.steps.insert(def.reference(), def);
        Ok(())
    }

    /// Add a recipe; it is checked against the verbs already here.
    /// Verbs share one namespace: a recipe may not take a step's name.
    pub fn add_workflow(&mut self, def: WorkflowDefinition) -> Result<(), Error> {
        self.check_workflow(&def)?;
        if self.steps.contains_key(&def.reference()) {
            return Err(Error::Invalid(format!("workflow definition {}: a step definition has that name and version; verbs share one namespace", def.reference())));
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
                return Err(Error::Invalid(format!("workflow definition {}: a step definition has that name and version; verbs share one namespace", w.reference())));
            }
            lib.workflows.insert(w.reference(), w);
        }
        for w in lib.workflows.values() {
            lib.check_workflow(w)?;
        }
        Ok(lib)
    }

    /// A verb: a StepDefinition, or a WorkflowDefinition seen from
    /// outside (its derived face), so recipes nest.
    pub fn step(&self, r: &Ref) -> Option<StepDefinition> {
        self.steps.get(r).cloned().or_else(|| self.workflows.get(r).map(|w| self.face_of(w)))
    }

    pub fn workflow(&self, r: &Ref) -> Option<&WorkflowDefinition> {
        self.workflows.get(r)
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
            let Some(def) = self.step(&entry.step) else { continue };
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

    /// Names unique and undotted, every entry's verb known, every
    /// Binding between ports that exist with equal tags, every Guard
    /// over a step and Decision that exist, no cycles.
    pub fn check_workflow(&self, w: &WorkflowDefinition) -> Result<(), Error> {
        let invalid = |m: String| Error::Invalid(format!("workflow definition {}: {m}", w.reference()));
        if w.contract != crate::definition::WORKFLOW_DEFINITION {
            return Err(invalid(format!("contract is {:?}", w.contract)));
        }
        if w.steps.is_empty() {
            return Err(invalid("no steps".into()));
        }
        let mut defs: BTreeMap<&str, StepDefinition> = BTreeMap::new();
        for StepEntry { name, step, .. } in &w.steps {
            if name.is_empty() || name.contains('.') {
                return Err(invalid(format!("step name {name:?} is empty or contains a dot")));
            }
            if defs.contains_key(name.as_str()) {
                return Err(invalid(format!("step name {name:?} is used twice")));
            }
            if step == &w.reference() {
                return Err(invalid(format!("step {name:?} is the recipe itself")));
            }
            let def = self.step(step).ok_or_else(|| invalid(format!("step {name:?} refers to {step}, which is not in the library")))?;
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
