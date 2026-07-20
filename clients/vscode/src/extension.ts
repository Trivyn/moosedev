// MOOSEDev Knowledge-LSP client for VS Code (spec §5.7: a thin "picture
// frame" — LSP client registration only, no logic of its own).
//
// The server is the `moosedev` binary's stdio shim; it relays to the shared
// daemon and autospawns it on first use. Language intelligence stays with
// rust-analyzer / tsserver — MOOSEDev only adds hover dossiers, code lenses,
// knowledge diagnostics, and the proposal code actions.
//
// One client per `.moosedev` workspace folder: each daemon owns exactly one
// project graph, so in a multi-root workspace every knowledge root gets its
// own client whose documentSelector is scoped to that folder — files never
// leak to a sibling root's daemon. Within a folder there is no language
// filter: the server is silent for files outside its indexing substrate, so
// new substrate languages need no client edit.
//
// `moosedev.openEntity` (the code-lens command) needs no registration here:
// the server lists it in executeCommandProvider, vscode-languageclient
// forwards it via workspace/executeCommand, and the server opens the
// workbench through window/showDocument.

import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";

const clients = new Map<string, LanguageClient>();
let extensionActive = false;

// Per-folder lifecycle queue: start/stop for the same folder run strictly in
// sequence. Without it, a folder-removal or configuration event racing an
// in-flight start() would try to stop a client in the Starting state
// (vscode-languageclient rejects that), leaving the finished startup running
// untracked — and a config restart would then create a duplicate.
const lifecycle = new Map<string, Promise<void>>();

function withFolderQueue(key: string, op: () => Promise<void>): Promise<void> {
  const queued = (lifecycle.get(key) ?? Promise.resolve()).then(op, op);
  const cleanup = queued.finally(() => {
    if (lifecycle.get(key) === cleanup) {
      lifecycle.delete(key);
    }
  });
  lifecycle.set(key, cleanup);
  return cleanup;
}

function isKnowledgeFolder(folder: vscode.WorkspaceFolder): boolean {
  return fs.existsSync(path.join(folder.uri.fsPath, ".moosedev"));
}

// Must mirror the server's InitializationOptions shape (src/lsp/mod.rs).
function initializationOptions(config: vscode.WorkspaceConfiguration) {
  return {
    diagnostics: {
      constraints: config.get<boolean>("diagnostics.constraints", true),
      staleRationale: config.get<boolean>("diagnostics.staleRationale", true),
    },
    codeLens: config.get<boolean>("codeLens", true),
    nudge: config.get<boolean>("nudge", true),
  };
}

function startClient(folder: vscode.WorkspaceFolder): Promise<void> {
  const key = folder.uri.toString();
  return withFolderQueue(key, () => doStartClient(key, folder));
}

async function doStartClient(
  key: string,
  folder: vscode.WorkspaceFolder
): Promise<void> {
  const folderStillOpen = vscode.workspace.workspaceFolders?.some(
    (candidate) => candidate.uri.toString() === key
  );
  if (
    !extensionActive ||
    !folderStillOpen ||
    clients.has(key) ||
    !isKnowledgeFolder(folder)
  ) {
    return;
  }
  const config = vscode.workspace.getConfiguration("moosedev", folder.uri);
  const command = config.get<string>("serverPath", "").trim() || "moosedev";

  const serverOptions: ServerOptions = {
    command,
    args: ["lsp"],
    // cwd is load-bearing: the autospawned daemon derives its repo root and
    // data dir from the working directory.
    options: { cwd: folder.uri.fsPath },
  };

  const clientOptions: LanguageClientOptions = {
    // Scoped to this folder so multi-root workspaces route each file to its
    // own root's daemon. A relative pattern treats the base as a literal
    // path — interpolating fsPath into a glob string would break on folders
    // whose names contain glob characters (e.g. "project[old]"). This is the
    // protocol shape; the client converts it to a vscode.RelativePattern.
    documentSelector: [
      {
        scheme: "file",
        pattern: { baseUri: folder.uri.toString(), pattern: "**/*" },
      },
    ],
    workspaceFolder: folder,
    initializationOptions: initializationOptions(config),
    diagnosticPullOptions: { onChange: true, onSave: true },
  };

  const client = new LanguageClient(
    "moosedev",
    `MOOSEDev Knowledge-LSP (${folder.name})`,
    serverOptions,
    clientOptions
  );
  // Keep an in-flight client visible to stopAll(). The per-folder queue makes
  // this safe: a queued stop cannot call stop() until start() has settled.
  clients.set(key, client);
  try {
    await client.start();
  } catch (error) {
    if (clients.get(key) === client) {
      clients.delete(key);
    }
    // dispose() is asynchronous and can reject for StartFailed clients.
    try {
      await client.dispose();
    } catch {
      // Best-effort cleanup of a client that never reached Running.
    }
    void vscode.window.showErrorMessage(
      "MOOSEDev: could not start the Knowledge-LSP — install moosedev and " +
        "ensure it is on PATH, or set moosedev.serverPath. " +
        `(${error instanceof Error ? error.message : String(error)})`
    );
  }
}

function stopClient(key: string): Promise<void> {
  return withFolderQueue(key, async () => {
    const client = clients.get(key);
    clients.delete(key);
    if (client) {
      await client.stop().catch(() => undefined);
    }
  });
}

async function startAll(): Promise<void> {
  // Register every folder queue before awaiting any one startup, so stopAll()
  // and deactivation cannot miss later roots in a multi-root workspace.
  await Promise.all((vscode.workspace.workspaceFolders ?? []).map(startClient));
}

async function stopAll(): Promise<void> {
  for (const key of [...clients.keys()]) {
    await stopClient(key);
  }
}

export function activate(context: vscode.ExtensionContext): void {
  extensionActive = true;
  void startAll();

  context.subscriptions.push(
    // initializationOptions are read once at initialize; a settings change
    // needs a restart to take effect.
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (event.affectsConfiguration("moosedev")) {
        await stopAll();
        await startAll();
      }
    }),
    vscode.workspace.onDidChangeWorkspaceFolders(async (event) => {
      for (const removed of event.removed) {
        await stopClient(removed.uri.toString());
      }
      for (const added of event.added) {
        await startClient(added);
      }
    })
  );
}

export async function deactivate(): Promise<void> {
  // Prevent an activate-time startAll() loop from advancing to another folder
  // after the currently queued startup settles.
  extensionActive = false;
  await stopAll();
}
