import { useEffect, useState } from "react";
import { Channel, invoke } from "@tauri-apps/api/core";
import type { ConversationEvent, EventRef } from "./events";
import { describe } from "./events";

/**
 * The least UI that exercises the Claude Code backend from the shell: open a conversation
 * under an account name in a workspace, send input, watch every event the backend
 * reports, interrupt, terminate, resume. Two conversations side by side is the point
 * (#46). Nothing here answers an approval; the dialog is the shell's, and until it is
 * wired the gate refuses every delegated call for want of a presenter that can show it —
 * which arrives here as a diagnostic and an error tool result, not as an approval event.
 */

interface Conversation {
  id: number;
  account: string;
  workspaceRoot: string;
  lines: Line[];
  exited: boolean;
  hasSession: boolean;
}

interface Line {
  key: number;
  event: EventRef;
}

let nextKey = 1;

export function App() {
  const [version, setVersion] = useState<string | null>(null);
  const [backendError, setBackendError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [account, setAccount] = useState("");
  const [workspaceRoot, setWorkspaceRoot] = useState("");
  const [conversations, setConversations] = useState<Conversation[]>([]);

  useEffect(() => {
    invoke<string>("core_version")
      .then(setVersion)
      .catch((cause: unknown) => setError(String(cause)));
    invoke<string | null>("backend_status")
      .then(setBackendError)
      .catch((cause: unknown) => setError(String(cause)));
  }, []);

  const update = (id: number, f: (c: Conversation) => Conversation) =>
    setConversations((all) => all.map((c) => (c.id === id ? f(c) : c)));

  const start = async () => {
    setError(null);
    const channel = new Channel<ConversationEvent>();
    let id: number | null = null;
    const pending: EventRef[] = [];
    channel.onmessage = ({ conversation, event }) => {
      if (id === null) {
        pending.push(event);
        return;
      }
      append(conversation, event);
    };
    try {
      id = await invoke<number>("start_conversation", { account, workspaceRoot, channel });
      setConversations((all) => [
        ...all,
        { id: id!, account, workspaceRoot, lines: [], exited: false, hasSession: false },
      ]);
      for (const event of pending) append(id, event);
    } catch (cause) {
      setError(String(cause));
    }
  };

  const append = (id: number, event: EventRef) =>
    update(id, (c) => ({
      ...c,
      lines: merge(c.lines, event),
      exited: event.kind === "exited" ? true : event.kind === "session_opened" ? false : c.exited,
      hasSession: c.hasSession || event.kind === "session_opened",
    }));

  const call = async (command: string, args: Record<string, unknown>) => {
    setError(null);
    try {
      await invoke(command, args);
      return true;
    } catch (cause) {
      setError(String(cause));
      return false;
    }
  };

  return (
    <main>
      <header>
        <h1>stanchion</h1>
        <p>core {version ?? "…"}</p>
        {backendError !== null && <p role="alert">Claude Code backend unavailable: {backendError}</p>}
        {error !== null && <p role="alert">{error}</p>}
      </header>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void start();
        }}
      >
        <input
          placeholder="account (a name of your choosing)"
          value={account}
          onChange={(e) => setAccount(e.target.value)}
        />
        <input
          placeholder="workspace root (absolute path)"
          value={workspaceRoot}
          onChange={(e) => setWorkspaceRoot(e.target.value)}
        />
        <button type="submit" disabled={backendError !== null || !account || !workspaceRoot}>
          Start conversation
        </button>
      </form>
      <section className="conversations">
        {conversations.map((c) => (
          <ConversationView key={c.id} conversation={c} call={call} />
        ))}
      </section>
    </main>
  );
}

/**
 * A turn's partial deltas are one line that grows, and the complete message replaces it;
 * every other event is a line of its own.
 */
function merge(lines: Line[], event: EventRef): Line[] {
  const last = lines[lines.length - 1];
  if (last?.event.kind === "message_partial" && last.event.turn === ("turn" in event ? event.turn : null)) {
    if (event.kind === "message_partial") {
      return [...lines.slice(0, -1), { key: last.key, event: { ...event, text: last.event.text + event.text } }];
    }
    if (event.kind === "message_complete" && event.message.role === "assistant") {
      return [...lines.slice(0, -1), { key: last.key, event }];
    }
  }
  return [...lines, { key: nextKey++, event }];
}

function ConversationView({
  conversation: c,
  call,
}: {
  conversation: Conversation;
  call: (command: string, args: Record<string, unknown>) => Promise<boolean>;
}) {
  const [text, setText] = useState("");
  return (
    <article>
      <h2>
        #{c.id} · {c.account}
      </h2>
      <p className="path">{c.workspaceRoot}</p>
      <ol className="transcript">
        {c.lines.map((l) => (
          <li key={l.key} className={l.event.kind}>
            {describe(l.event)}
          </li>
        ))}
      </ol>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (!text) return;
          // Cleared only once the core took it: a `Busy` refusal keeps the draft.
          void call("send_input", { conversation: c.id, text }).then((sent) => {
            if (sent) setText((current) => (current === text ? "" : current));
          });
        }}
      >
        <input value={text} onChange={(e) => setText(e.target.value)} disabled={c.exited} placeholder="say something" />
        <button type="submit" disabled={c.exited}>
          Send
        </button>
        <button type="button" onClick={() => void call("interrupt_conversation", { conversation: c.id })} disabled={c.exited}>
          Interrupt
        </button>
        <button type="button" onClick={() => void call("terminate_conversation", { conversation: c.id })} disabled={c.exited}>
          Terminate
        </button>
        <button
          type="button"
          onClick={() => void call("resume_conversation", { conversation: c.id })}
          disabled={!c.exited || !c.hasSession}
        >
          Resume
        </button>
      </form>
    </article>
  );
}
