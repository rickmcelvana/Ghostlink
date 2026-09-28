with open("crates/ghost-link/src/task_api.rs", "r") as f:
    text = f.read()

old_call = """    match crate::task_runtime::TaskRunner::spawn_implementer(
        store,
        task,
        role,
        model,
        payload.brief,
        cancel_token,
    )"""

new_call = """    let backend = Arc::new(RealAgentBackend {
        state: Arc::clone(&state),
    });

    match crate::task_runtime::TaskRunner::spawn_implementer(
        store,
        backend,
        task,
        role,
        model,
        payload.brief,
        cancel_token,
    )"""

text = text.replace(old_call, new_call)

with open("crates/ghost-link/src/task_api.rs", "w") as f:
    f.write(text)

print("Fixed spawn_implementer call arguments in task_api.rs")
