use anyhow::{bail, Result};
use crossbeam_channel::{Receiver, Sender};
use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::{
    notification::{Exit, Initialized},
    request::{Initialize, Shutdown},
    ClientCapabilities, ClientInfo, DidOpenTextDocumentParams, InitializeParams, InitializeResult,
    InitializedParams, Url,
};
use tempfile::{tempdir, TempDir};
use texlab::Server;

pub struct IncomingHandler {
    _handle: jod_thread::JoinHandle<Result<()>>,
    pub requests: Receiver<Request>,
    pub notifications: Receiver<Notification>,
    pub responses: Receiver<Response>,
}

impl IncomingHandler {
    pub fn spawn(receiver: Receiver<Message>) -> Result<Self> {
        let (req_sender, req_receiver) = crossbeam_channel::unbounded();
        let (not_sender, not_receiver) = crossbeam_channel::unbounded();
        let (res_sender, res_receiver) = crossbeam_channel::unbounded();

        let _handle = jod_thread::spawn(move || {
            for message in &receiver {
                match message {
                    Message::Request(req) => req_sender.send(req)?,
                    Message::Response(res) => res_sender.send(res)?,
                    Message::Notification(not) => not_sender.send(not)?,
                };
            }

            Ok(())
        });

        Ok(Self {
            _handle,
            requests: req_receiver,
            notifications: not_receiver,
            responses: res_receiver,
        })
    }
}

pub struct ClientResult {
    pub directory: TempDir,
    pub incoming: IncomingHandler,
}

pub struct Client {
    outgoing: Sender<Message>,
    incoming: IncomingHandler,
    directory: TempDir,
    request_id: i32,
    _handle: jod_thread::JoinHandle,
}

impl Client {
    pub fn spawn() -> Result<Self> {
        let directory = tempdir()?;
        let (client, server) = Connection::memory();
        let incoming = IncomingHandler::spawn(client.receiver)?;
        let outgoing = client.sender;
        let server = Server::with_connection(server, directory.path().to_path_buf(), false);
        let _handle = jod_thread::spawn(move || {
            server.run().expect("server failed to run");
        });

        Ok(Self {
            outgoing,
            incoming,
            directory,
            request_id: 0,
            _handle,
        })
    }

    #[allow(deprecated)]
    pub fn initialize(
        &mut self,
        client_capabilities: ClientCapabilities,
        client_info: Option<ClientInfo>,
    ) -> Result<InitializeResult> {
        let result = self.request::<Initialize>(InitializeParams {
            process_id: None,
            root_path: None,
            root_uri: None,
            initialization_options: None,
            capabilities: client_capabilities,
            trace: None,
            workspace_folders: None,
            client_info,
            locale: None,
        })?;

        self.notify::<Initialized>(InitializedParams {})?;
        Ok(result)
    }

    pub fn request<R: lsp_types::request::Request>(
        &mut self,
        params: R::Params,
    ) -> Result<R::Result> {
        self.request_id += 1;

        self.outgoing
            .send(Request::new(self.request_id.into(), R::METHOD.into(), params).into())?;

        let response = self.incoming.responses.recv()?;
        assert_eq!(response.id, self.request_id.into());

        let result = match response.result {
            Some(result) => result,
            None => bail!("request failed: {:?}", response.error),
        };

        Ok(serde_json::from_value(result)?)
    }

    pub fn notify<N: lsp_types::notification::Notification>(
        &mut self,
        params: N::Params,
    ) -> Result<()> {
        self.outgoing
            .send(Notification::new(N::METHOD.into(), serde_json::to_value(params)?).into())?;

        Ok(())
    }

    pub fn open(&mut self, name: &str, language_id: &str, text: String) -> Result<()> {
        self.notify::<lsp_types::notification::DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: lsp_types::TextDocumentItem {
                uri: self.uri(name)?,
                language_id: language_id.to_string(),
                version: 0,
                text,
            },
        })?;

        Ok(())
    }

    pub fn shutdown(mut self) -> Result<ClientResult> {
        self.request::<Shutdown>(())?;
        self.notify::<Exit>(())?;
        Ok(ClientResult {
            directory: self.directory,
            incoming: self.incoming,
        })
    }

    pub fn uri(&self, name: &str) -> Result<Url> {
        Url::from_file_path(self.directory.path().join(name))
            .map_err(|()| anyhow::anyhow!("failed to create uri"))
    }
}
