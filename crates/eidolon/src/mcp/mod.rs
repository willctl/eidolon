//! The browser's tools for an agent, as an MCP server on standard input and
//! output. They name what they act on by short uids from a snapshot. There
//! is no tool that runs arbitrary script.

pub mod files;
mod keys;
pub mod snapshot;
mod tools;

pub use tools::{args, check_url, definitions, Download, Part, Tool, ToolDef, Tools};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::IntoTransport;
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt};
use std::borrow::Cow;
use std::future::Future;
use std::sync::Arc;

const NAME: &str = "eidolon-browser";
const INSTRUCTIONS: &str = "Tools for the web browser of one identity. Call take_snapshot to see a page and get a uid for each element, then act on a uid. Files go through the shared files folder: upload_file_by_uid takes a file from it, fetch_download puts a download in its downloads folder.";

struct Server {
    tools: Arc<Tools>,
    listed: Vec<rmcp::model::Tool>,
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    fn list_tools(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(self.listed.clone())))
    }

    async fn call_tool(&self, request: CallToolRequestParams, _context: RequestContext<RoleServer>) -> Result<CallToolResponse, McpError> {
        if !self.listed.iter().any(|t| t.name == request.name) {
            return Err(McpError::invalid_params(format!("There is no tool named {}.", request.name), None));
        }
        let arguments = request.arguments.map_or(serde_json::Value::Null, serde_json::Value::Object);
        // A tool that fails answers with its reason for the agent to act on; it is not a protocol error.
        let result = match Tool::parse(&request.name, arguments) {
            Ok(tool) => self.tools.run(tool).await,
            Err(e) => Err(e),
        };
        let answer = match result {
            Ok(parts) => CallToolResult::success(
                parts
                    .into_iter()
                    .map(|p| match p {
                        Part::Text(t) => ContentBlock::text(t),
                        Part::Image { data, mime } => ContentBlock::image(data, mime),
                    })
                    .collect(),
            ),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
        };
        Ok(answer.into())
    }
}

pub async fn serve<T, E, A>(tools: Tools, transport: T) -> crate::Result<()>
where
    T: IntoTransport<RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let listed = definitions().into_iter().map(|d| rmcp::model::Tool::new(Cow::Borrowed(d.name), Cow::Borrowed(d.description), Arc::new(d.schema))).collect();
    let server = Server { tools: Arc::new(tools), listed };
    match server.serve(transport).await {
        Ok(running) => {
            let _ = running.waiting().await;
            Ok(())
        }
        Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => Ok(()),
        Err(e) => Err(crate::Error::Mcp(e.to_string())),
    }
}

pub async fn serve_stdio(tools: Tools) -> crate::Result<()> {
    serve(tools, rmcp::transport::stdio()).await
}
