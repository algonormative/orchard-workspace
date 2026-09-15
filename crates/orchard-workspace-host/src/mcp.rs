use orchard_mail_mcp::{CoreBackend, ToolBackend, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Weak;

use crate::{call_from_mcp, HostInner};

pub(crate) struct CombinedBackend {
    host: Weak<HostInner>,
    workspace_id: String,
    mail: CoreBackend,
}

impl CombinedBackend {
    pub(crate) fn new(host: Weak<HostInner>, workspace_id: String, mail: CoreBackend) -> Self {
        Self {
            host,
            workspace_id,
            mail,
        }
    }
}

impl ToolBackend for CombinedBackend {
    fn tools(&self) -> Vec<ToolDefinition> {
        let mut tools = self.mail.tools();
        tools.extend(task_tools());
        tools
    }

    fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "tasks_list" | "workspace_info" | "task_show" | "task_create" | "task_update"
            | "task_close" | "task_dependencies" => {
                call_from_mcp(&self.host, &self.workspace_id, name, args)
            }
            _ => self.mail.call(name, args),
        }
    }
}

fn task_tools() -> Vec<ToolDefinition> {
    vec![
        tool(
            "workspace_info",
            "Discover this endpoint's workspace, participants, attached repositories and task stores, and available capabilities. Credentials and other workspaces are never returned.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        tool(
            "tasks_list",
            "List tasks in one attached Beads store. Task identities are qualified by store_id.",
            json!({
                "type":"object",
                "properties":{"store_id":{"type":"string"},"status":{"type":"string"}},
                "required":["store_id"],"additionalProperties":false
            }),
        ),
        tool(
            "task_show",
            "Show one task from one attached Beads store.",
            task_ref_schema(),
        ),
        tool(
            "task_create",
            "Create one task idempotently. Reuse request_id after a lost response.",
            json!({
                "type":"object",
                "properties":{
                    "store_id":{"type":"string"},"request_id":{"type":"string"},
                    "title":{"type":"string"},"description":{"type":"string"},
                    "priority":{"type":"integer","minimum":0,"maximum":4},
                    "labels":{"type":"array","items":{"type":"string"}}
                },
                "required":["store_id","request_id","title"],"additionalProperties":false
            }),
        ),
        tool(
            "task_update",
            "Update only supplied task fields. Omitted description, dependencies, and labels are preserved.",
            json!({
                "type":"object",
                "properties":{
                    "store_id":{"type":"string"},"task_id":{"type":"string"},
                    "request_id":{"type":"string"},"description":{"type":"string"},
                    "title":{"type":"string"},
                    "status":{"type":"string"},
                    "priority":{"type":"integer","minimum":0,"maximum":4},
                    "add_labels":{"type":"array","items":{"type":"string"}},
                    "remove_labels":{"type":"array","items":{"type":"string"}}
                },
                "required":["store_id","task_id","request_id"],"additionalProperties":false
            }),
        ),
        tool(
            "task_close",
            "Close one task. Reuse request_id after a lost response and inspect reconciled responses.",
            json!({
                "type":"object",
                "properties":{
                    "store_id":{"type":"string"},"task_id":{"type":"string"},
                    "request_id":{"type":"string"},"reason":{"type":"string"}
                },
                "required":["store_id","task_id","request_id"],"additionalProperties":false
            }),
        ),
        tool(
            "task_dependencies",
            "Read both dependency directions for one qualified task.",
            task_ref_schema(),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
    }
}

fn task_ref_schema() -> Value {
    json!({
        "type":"object",
        "properties":{"store_id":{"type":"string"},"task_id":{"type":"string"}},
        "required":["store_id","task_id"],"additionalProperties":false
    })
}
