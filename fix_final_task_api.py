with open("crates/ghost-link/src/task_api.rs", "r") as f:
    text = f.read()

text = text.replace("task_runtime::TaskRunner", "crate::task_runtime::TaskRunner")

with open("crates/ghost-link/src/task_api.rs", "w") as f:
    f.write(text)

print("Fixed task_runtime namespace in task_api.rs")
