use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use rmcp::model::{
    AnnotateAble, CallToolRequestParams, CallToolResult, Implementation, InitializeResult,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, RawResource,
    ReadResourceRequestParams, ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo,
    Tool,
};
use rmcp::service::{RequestContext, RoleServer};

use crate::state::AgentState;
use crate::tools::{all_tools, dispatch_tool};

#[derive(Clone)]
pub struct AutomataMcpServer {
    pub state: Arc<AgentState>,
    tools: Arc<Vec<Tool>>,
}

impl AutomataMcpServer {
    pub fn new(state: Arc<AgentState>) -> Self {
        Self {
            state,
            tools: Arc::new(all_tools()),
        }
    }
}

impl ServerHandler for AutomataMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = InitializeResult::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        );
        info.server_info = Implementation::new("ui-automata", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "UI-Automata Linux MCP server. Prefer AT-SPI2 desktop/run_actions selectors before vision OCR/coordinate input; inspect resources `linux/AGENT.md` or workflow YAMLs before writing workflows."
                .into(),
        );
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items((*self.tools).clone()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|t| t.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = serde_json::Value::Object(request.arguments.unwrap_or_default());
        dispatch_tool(request.name.as_ref(), args, self.state.clone(), context).await
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let names = self.state.list_resource_names();
        let resources = names
            .into_iter()
            .map(|name| {
                let mime = if name.ends_with(".md") {
                    "text/markdown"
                } else {
                    "text/yaml"
                };
                let mut raw = RawResource::new(format!("automata://{name}"), name);
                raw.mime_type = Some(mime.into());
                raw.no_annotation()
            })
            .collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let content = self
            .state
            .read_resource_content(&request.uri)
            .map_err(|e| McpError::resource_not_found(e.to_string(), None))?;
        let mime = if request.uri.ends_with(".md") {
            "text/markdown"
        } else {
            "text/yaml"
        };
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(content, request.uri).with_mime_type(mime),
        ]))
    }
}
