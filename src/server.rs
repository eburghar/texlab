use crate::action::{Action, ActionMananger};
use crate::build::*;
use crate::client::LspClient;
use crate::completion::{CompletionItemData, CompletionProvider};
use crate::data::citation::render_citation;
use crate::data::completion::{LatexComponentDatabase, LatexComponentDatabaseManager};
use crate::data::component::ComponentDocumentation;
use crate::definition::DefinitionProvider;
use crate::diagnostics::{DiagnosticsManager, LatexLintOptions};
use crate::feature::{DocumentView, FeatureProvider, FeatureRequest};
use crate::folding::FoldingProvider;
use crate::formatting::bibtex::{self, BibtexFormattingOptions, BibtexFormattingParams};
use crate::forward_search::{self, ForwardSearchOptions, ForwardSearchResult};
use crate::highlight::HighlightProvider;
use crate::hover::HoverProvider;
use crate::link::LinkProvider;
use crate::reference::ReferenceProvider;
use crate::rename::{PrepareRenameProvider, RenameProvider};
use crate::request;
use crate::syntax::bibtex::BibtexDeclaration;
use crate::syntax::text::SyntaxNode;
use crate::syntax::{Language, SyntaxTree};
use crate::tex::resolver::{self, TexResolver, TEX_RESOLVER};
use crate::workspace::WorkspaceManager;
use futures::lock::Mutex;
use futures_boxed::boxed;
use jsonrpc::server::Result;
use jsonrpc_derive::{jsonrpc_method, jsonrpc_server};
use log::*;
use lsp_types::*;
use once_cell::sync::OnceCell;
use runtime::task::JoinHandle;
use serde::de::DeserializeOwned;
use std::ffi::OsStr;
use std::fs;
use std::mem;
use std::path::PathBuf;
use std::sync::Arc;
use walkdir::WalkDir;

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ServerConfig {
    pub component_database_path: PathBuf,
}

impl Default for ServerConfig {
    fn default() -> Self {
        let directory = dirs::home_dir()
            .expect("Could not find HOME directory")
            .join(".texlab");
        if !directory.exists() {
            fs::create_dir(&directory).expect("Could not create server directory");
        }

        Self {
            component_database_path: directory.join("components.json"),
        }
    }
}

pub struct LatexLspServer<C> {
    config: ServerConfig,
    client: Arc<C>,
    client_capabilities: OnceCell<Arc<ClientCapabilities>>,
    workspace_manager: WorkspaceManager,
    action_manager: ActionMananger,
    database_manager: OnceCell<Arc<LatexComponentDatabaseManager<C>>>,
    database_listener: Mutex<Option<JoinHandle<()>>>,
    diagnostics_manager: Mutex<DiagnosticsManager>,
    resolver: Mutex<Arc<TexResolver>>,
    completion_provider: CompletionProvider,
    definition_provider: DefinitionProvider,
    folding_provider: FoldingProvider,
    highlight_provider: HighlightProvider,
    hover_provider: HoverProvider,
    link_provider: LinkProvider,
    reference_provider: ReferenceProvider,
    prepare_rename_provider: PrepareRenameProvider,
    rename_provider: RenameProvider,
}

#[jsonrpc_server]
impl<C: LspClient + Send + Sync + 'static> LatexLspServer<C> {
    pub fn new(client: Arc<C>, config: ServerConfig) -> Self {
        LatexLspServer {
            config,
            client,
            client_capabilities: OnceCell::new(),
            workspace_manager: WorkspaceManager::default(),
            action_manager: ActionMananger::default(),
            database_manager: OnceCell::new(),
            database_listener: Mutex::default(),
            diagnostics_manager: Mutex::new(DiagnosticsManager::default()),
            resolver: Mutex::new(Arc::new(TexResolver::default())),
            completion_provider: CompletionProvider::new(),
            definition_provider: DefinitionProvider::new(),
            folding_provider: FoldingProvider::new(),
            highlight_provider: HighlightProvider::new(),
            hover_provider: HoverProvider::new(),
            link_provider: LinkProvider::new(),
            reference_provider: ReferenceProvider::new(),
            prepare_rename_provider: PrepareRenameProvider::new(),
            rename_provider: RenameProvider::new(),
        }
    }

    #[jsonrpc_method("initialize", kind = "request")]
    pub async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        self.client_capabilities
            .set(Arc::new(params.capabilities))
            .unwrap();
        let capabilities = ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Options(
                TextDocumentSyncOptions {
                    open_close: Some(true),
                    change: Some(TextDocumentSyncKind::Full),
                    will_save: None,
                    will_save_wait_until: None,
                    save: Some(SaveOptions {
                        include_text: Some(false),
                    }),
                },
            )),
            hover_provider: Some(true),
            completion_provider: Some(CompletionOptions {
                resolve_provider: Some(true),
                trigger_characters: Some(vec!['\\', '{', '}', '@', '/', ' ']),
            }),
            signature_help_provider: None,
            definition_provider: Some(true),
            type_definition_provider: None,
            implementation_provider: None,
            references_provider: Some(true),
            document_highlight_provider: Some(true),
            document_symbol_provider: None,
            workspace_symbol_provider: None,
            code_action_provider: None,
            code_lens_provider: None,
            document_formatting_provider: Some(true),
            document_range_formatting_provider: None,
            document_on_type_formatting_provider: None,
            rename_provider: Some(RenameProviderCapability::Options(RenameOptions {
                prepare_provider: Some(true),
            })),
            document_link_provider: Some(DocumentLinkOptions {
                resolve_provider: Some(false),
            }),
            color_provider: None,
            folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
            execute_command_provider: None,
            workspace: None,
            selection_range_provider: None,
        };

        Ok(InitializeResult { capabilities })
    }

    #[jsonrpc_method("initialized", kind = "notification")]
    pub fn initialized(&self, _params: InitializedParams) {
        self.action_manager.push(Action::RegisterCapabilities);
        self.action_manager.push(Action::LoadResolver);
        self.action_manager.push(Action::LoadComponentDatabase);
        self.action_manager.push(Action::ScanComponents);
        self.action_manager.push(Action::DetectChildren);
        self.action_manager.push(Action::PublishDiagnostics);
    }

    #[jsonrpc_method("shutdown", kind = "request")]
    pub async fn shutdown(&self, _params: ()) -> Result<()> {
        Ok(())
    }

    #[jsonrpc_method("exit", kind = "notification")]
    pub fn exit(&self, _params: ()) {}

    #[jsonrpc_method("$/cancelRequest", kind = "notification")]
    pub fn cancel_request(&self, _params: CancelParams) {}

    #[jsonrpc_method("workspace/didChangeWatchedFiles", kind = "notification")]
    pub fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let workspace = self.workspace_manager.get();
        for change in params.changes {
            if change.uri.scheme() != "file" {
                continue;
            }

            let log_path = change.uri.to_file_path().unwrap();
            let name = log_path.to_string_lossy().into_owned();
            let tex_path = PathBuf::from(format!("{}tex", &name[0..name.len() - 3]));
            let tex_uri = Uri::from_file_path(tex_path).unwrap();
            if workspace.find(&tex_uri).is_some() {
                self.action_manager
                    .push(Action::ParseLog { tex_uri, log_path });
            }
        }
        self.action_manager.push(Action::PublishDiagnostics);
    }

    #[jsonrpc_method("textDocument/didOpen", kind = "notification")]
    pub fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        self.workspace_manager.add(params.text_document);
        self.action_manager.push(Action::DetectRoot(uri));
        self.action_manager.push(Action::DetectChildren);
        self.action_manager.push(Action::ScanComponents);
        self.action_manager.push(Action::PublishDiagnostics);
    }

    #[jsonrpc_method("textDocument/didChange", kind = "notification")]
    pub fn did_change(&self, params: DidChangeTextDocumentParams) {
        for change in params.content_changes {
            let uri = params.text_document.uri.clone();
            self.workspace_manager.update(uri, change.text);
        }
        self.action_manager.push(Action::DetectChildren);
        self.action_manager.push(Action::ScanComponents);
        self.action_manager.push(Action::PublishDiagnostics);
    }

    #[jsonrpc_method("textDocument/didSave", kind = "notification")]
    pub fn did_save(&self, params: DidSaveTextDocumentParams) {
        self.action_manager
            .push(Action::RunLinter(params.text_document.uri.clone()));
        self.action_manager.push(Action::PublishDiagnostics);
        self.action_manager
            .push(Action::Build(params.text_document.uri));
    }

    #[jsonrpc_method("textDocument/didClose", kind = "notification")]
    pub fn did_close(&self, _params: DidCloseTextDocumentParams) {}

    #[jsonrpc_method("textDocument/completion", kind = "request")]
    pub async fn completion(&self, params: CompletionParams) -> Result<CompletionList> {
        let request = request!(self, params)?;
        let items = self.completion_provider.execute(&request).await;
        Ok(CompletionList {
            is_incomplete: true,
            items,
        })
    }

    #[jsonrpc_method("completionItem/resolve", kind = "request")]
    pub async fn completion_resolve(&self, mut item: CompletionItem) -> Result<CompletionItem> {
        let data: CompletionItemData = serde_json::from_value(item.data.clone().unwrap()).unwrap();
        match data {
            CompletionItemData::Package | CompletionItemData::Class => {
                item.documentation = ComponentDocumentation::lookup(&item.label)
                    .await
                    .map(|documentation| Documentation::MarkupContent(documentation.content));
            }
            CompletionItemData::Citation { entry_code } => {
                if let Ok(markdown) = render_citation(&entry_code).await {
                    item.documentation = Some(Documentation::MarkupContent(markdown));
                }
            }
            _ => {}
        };
        Ok(item)
    }

    #[jsonrpc_method("textDocument/hover", kind = "request")]
    pub async fn hover(&self, params: TextDocumentPositionParams) -> Result<Option<Hover>> {
        let request = request!(self, params)?;
        let hover = self.hover_provider.execute(&request).await;
        Ok(hover)
    }

    #[jsonrpc_method("textDocument/definition", kind = "request")]
    pub async fn definition(&self, params: TextDocumentPositionParams) -> Result<Vec<Location>> {
        let request = request!(self, params)?;
        let results = self.definition_provider.execute(&request).await;
        Ok(results)
    }

    #[jsonrpc_method("textDocument/references", kind = "request")]
    pub async fn references(&self, params: ReferenceParams) -> Result<Vec<Location>> {
        let request = request!(self, params)?;
        let results = self.reference_provider.execute(&request).await;
        Ok(results)
    }

    #[jsonrpc_method("textDocument/documentHighlight", kind = "request")]
    pub async fn document_highlight(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Vec<DocumentHighlight>> {
        let request = request!(self, params)?;
        let results = self.highlight_provider.execute(&request).await;
        Ok(results)
    }

    #[jsonrpc_method("textDocument/documentSymbol", kind = "request")]
    pub async fn document_symbol(
        &self,
        _params: DocumentSymbolParams,
    ) -> Result<Vec<DocumentSymbol>> {
        Ok(Vec::new())
    }

    #[jsonrpc_method("textDocument/documentLink", kind = "request")]
    pub async fn document_link(&self, params: DocumentLinkParams) -> Result<Vec<DocumentLink>> {
        let request = request!(self, params)?;
        let links = self.link_provider.execute(&request).await;
        Ok(links)
    }

    #[jsonrpc_method("textDocument/formatting", kind = "request")]
    pub async fn formatting(&self, params: DocumentFormattingParams) -> Result<Vec<TextEdit>> {
        let request = request!(self, params)?;
        let mut edits = Vec::new();
        if let SyntaxTree::Bibtex(tree) = &request.document().tree {
            let params = BibtexFormattingParams {
                tab_size: request.params.options.tab_size as usize,
                insert_spaces: request.params.options.insert_spaces,
                options: self
                    .configuration::<BibtexFormattingOptions>("bibtex.formatting")
                    .await,
            };

            for declaration in &tree.root.children {
                let should_format = match declaration {
                    BibtexDeclaration::Comment(_) => false,
                    BibtexDeclaration::Preamble(_) | BibtexDeclaration::String(_) => true,
                    BibtexDeclaration::Entry(entry) => !entry.is_comment(),
                };
                if should_format {
                    let text = bibtex::format_declaration(&declaration, &params);
                    edits.push(TextEdit::new(declaration.range(), text.into()));
                }
            }
        }
        Ok(edits)
    }

    #[jsonrpc_method("textDocument/prepareRename", kind = "request")]
    pub async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<Range>> {
        let request = request!(self, params)?;
        let range = self.prepare_rename_provider.execute(&request).await;
        Ok(range)
    }

    #[jsonrpc_method("textDocument/rename", kind = "request")]
    pub async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let request = request!(self, params)?;
        let edit = self.rename_provider.execute(&request).await;
        Ok(edit)
    }

    #[jsonrpc_method("textDocument/foldingRange", kind = "request")]
    pub async fn folding_range(&self, params: FoldingRangeParams) -> Result<Vec<FoldingRange>> {
        let request = request!(self, params)?;
        let foldings = self.folding_provider.execute(&request).await;
        Ok(foldings)
    }

    #[jsonrpc_method("textDocument/build", kind = "request")]
    pub async fn build(&self, params: BuildParams) -> Result<BuildResult> {
        let request = request!(self, params)?;
        let options = self.configuration::<BuildOptions>("latex.build").await;
        let provider = BuildProvider::new(Arc::clone(&self.client), options);
        let result = provider.execute(&request).await;
        Ok(result)
    }

    #[jsonrpc_method("textDocument/forwardSearch", kind = "request")]
    pub async fn forward_search(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<ForwardSearchResult> {
        let request = request!(self, params)?;
        let options = self
            .configuration::<ForwardSearchOptions>("latex.forwardSearch")
            .await;

        let tex_file = request.document().uri.to_file_path().unwrap();
        let parent = request
            .workspace()
            .find_parent(&request.document().uri)
            .unwrap_or(request.view.document);
        let parent = parent.uri.to_file_path().unwrap();
        forward_search::search(&tex_file, &parent, request.params.position.line, options)
            .await
            .ok_or_else(|| format!("Unable to execute forward search"))
    }

    pub async fn stop_scanning(&self) {
        self.database_manager.get().unwrap().close().await;
        let mut listener = self.database_listener.lock().await;
        if let Some(handle) = mem::replace(&mut *listener, None) {
            handle.await;
        }
    }

    async fn configuration<T>(&self, section: &'static str) -> T
    where
        T: DeserializeOwned + Default,
    {
        if !self
            .client_capabilities
            .get()
            .and_then(|cap| cap.workspace.as_ref())
            .and_then(|cap| cap.configuration)
            .unwrap_or(false)
        {
            return T::default();
        }

        let params = ConfigurationParams {
            items: vec![ConfigurationItem {
                section: Some(section.into()),
                scope_uri: None,
            }],
        };

        match self.client.configuration(params).await {
            Ok(json) => match serde_json::from_value::<Vec<T>>(json) {
                Ok(config) => config.into_iter().next().unwrap(),
                Err(_) => {
                    warn!("Invalid configuration: {}", section);
                    T::default()
                }
            },
            Err(why) => {
                error!(
                    "Retrieving configuration for {} failed: {}",
                    section, why.message
                );
                T::default()
            }
        }
    }
}

impl<C: LspClient + Send + Sync + 'static> jsonrpc::ActionHandler for LatexLspServer<C> {
    #[boxed]
    async fn execute_actions(&self) {
        for action in self.action_manager.take() {
            match action {
                Action::RegisterCapabilities => {
                    if self
                        .client_capabilities
                        .get()
                        .and_then(|cap| cap.workspace.as_ref())
                        .and_then(|cap| cap.did_change_watched_files.as_ref())
                        .and_then(|cap| cap.dynamic_registration)
                        .unwrap_or(false)
                    {
                        let options = DidChangeWatchedFilesRegistrationOptions {
                            watchers: vec![FileSystemWatcher {
                                kind: Some(WatchKind::Create | WatchKind::Change),
                                glob_pattern: "**/*.log".into(),
                            }],
                        };

                        if let Err(why) = self
                            .client
                            .register_capability(RegistrationParams {
                                registrations: vec![Registration {
                                    id: "build-log-watcher".into(),
                                    method: "workspace/didChangeWatchedFiles".into(),
                                    register_options: Some(serde_json::to_value(options).unwrap()),
                                }],
                            })
                            .await
                        {
                            warn!(
                                "Client does not support dynamic capability registration: {}",
                                why.message
                            );
                        }
                    }
                }
                Action::LoadResolver => {
                    match &*TEX_RESOLVER {
                        Ok(res) => {
                            let mut resolver = self.resolver.lock().await;
                            *resolver = Arc::clone(&res);
                        }
                        Err(why) => {
                            let message = match why {
                                resolver::Error::KpsewhichNotFound => {
                                    "An error occurred while executing `kpsewhich`.\
                                     Please make sure that your distribution is in your PATH \
                                     environment variable and provides the `kpsewhich` tool."
                                }
                                resolver::Error::UnsupportedTexDistribution => {
                                    "Your TeX distribution is not supported."
                                }
                                resolver::Error::CorruptFileDatabase => {
                                    "The file database of your TeX distribution seems \
                                     to be corrupt. Please rebuild it and try again."
                                }
                            };

                            let params = ShowMessageParams {
                                message: message.into(),
                                typ: MessageType::Error,
                            };

                            self.client.show_message(params).await;
                        }
                    };
                }
                Action::LoadComponentDatabase => {
                    let resolver = self.resolver.lock().await;
                    let manager = Arc::new(LatexComponentDatabaseManager::load_or_create(
                        &self.config.component_database_path,
                        Arc::clone(&resolver),
                        Arc::clone(&self.client),
                    ));

                    {
                        let manager = Arc::clone(&manager);
                        let listener = runtime::spawn(async move { manager.listen().await });

                        *self.database_listener.lock().await = Some(listener);
                    }

                    self.database_manager.set(manager).ok().unwrap();
                }
                Action::DetectRoot(uri) => {
                    if uri.scheme() == "file" {
                        let mut path = uri.to_file_path().unwrap();
                        while path.pop() {
                            let workspace = self.workspace_manager.get();
                            if workspace.find_parent(&uri).is_some() {
                                break;
                            }

                            for entry in WalkDir::new(&path)
                                .min_depth(1)
                                .max_depth(1)
                                .into_iter()
                                .filter_map(std::result::Result::ok)
                                .filter(|entry| entry.file_type().is_file())
                                .filter(|entry| {
                                    entry
                                        .path()
                                        .extension()
                                        .and_then(OsStr::to_str)
                                        .and_then(Language::by_extension)
                                        .is_some()
                                })
                            {
                                if let Ok(parent_uri) = Uri::from_file_path(entry.path()) {
                                    if workspace.find(&parent_uri).is_none() {
                                        self.workspace_manager.load(entry.path());
                                    }
                                }
                            }
                        }
                    }
                }
                Action::DetectChildren => {
                    let workspace = self.workspace_manager.get();
                    workspace
                        .unresolved_includes()
                        .iter()
                        .for_each(|path| self.workspace_manager.load(&path));
                }
                Action::PublishDiagnostics => {
                    let workspace = self.workspace_manager.get();
                    let diagnostics_manager = self.diagnostics_manager.lock().await;
                    for document in &workspace.documents {
                        self.client
                            .publish_diagnostics(PublishDiagnosticsParams {
                                uri: document.uri.clone(),
                                diagnostics: diagnostics_manager.get(&document),
                            })
                            .await;
                    }
                }
                Action::RunLinter(uri) => {
                    let config: LatexLintOptions = self.configuration("latex.lint").await;
                    if config.on_save() {
                        let mut diagnostics_manager = self.diagnostics_manager.lock().await;
                        diagnostics_manager.latex.update(&uri);
                    }
                }
                Action::ParseLog { tex_uri, log_path } => {
                    if let Ok(log) = fs::read_to_string(&log_path) {
                        let mut diagnostics_manager = self.diagnostics_manager.lock().await;
                        diagnostics_manager.build.update(&tex_uri, &log);
                    }
                }
                Action::Build(uri) => {
                    let config: BuildOptions = self.configuration("latex.build").await;
                    if config.on_save() {
                        let text_document = TextDocumentIdentifier::new(uri);
                        self.build(BuildParams { text_document }).await.unwrap();
                    }
                }
                Action::ScanComponents => {
                    let workspace = self.workspace_manager.get();
                    if let Some(database) = self.database_manager.get() {
                        for document in &workspace.documents {
                            if let SyntaxTree::Latex(tree) = &document.tree {
                                for component in &tree.components {
                                    database.enqueue(component).await;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[macro_export]
macro_rules! request {
    ($server:expr, $params:expr) => {{
        let workspace = $server.workspace_manager.get();
        let resolver = $server.resolver.lock().await;
        let component_database = match $server.database_manager.get() {
            Some(manager) => manager.get().await,
            None => LatexComponentDatabase::default(),
        };
        let client_capabilities = $server
            .client_capabilities
            .get()
            .expect("Failed to retrieve client capabilities");

        if let Some(document) = workspace.find(&$params.text_document.uri) {
            Ok(FeatureRequest {
                params: $params,
                view: DocumentView::new(workspace, document),
                resolver: Arc::clone(&resolver),
                component_database,
                client_capabilities: Arc::clone(&client_capabilities),
            })
        } else {
            let msg = format!("Unknown document: {}", $params.text_document.uri);
            Err(msg)
        }
    }};
}