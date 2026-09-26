// The events the shell sends on a conversation's channel: the TypeScript side of
// `src-tauri/src/events.rs`. Rendered only; nothing here is an instruction to the core,
// and there is no message in the other direction that answers an approval.

export interface SessionRef {
  backend: string;
  account: string;
  workspace_root: string;
  value: string;
}

export interface RenderedRef {
  title: string;
  body: string;
  parsed: [string, string][];
  negative: string;
  affirmative: string;
}

export type UsageRef =
  | { kind: "not_reported" }
  | {
      kind: "reported";
      input_tokens: number;
      output_tokens: number;
      estimated_cost_micros: number | null;
    };

export type TurnEndRef =
  | { kind: "completed" }
  | { kind: "interrupted" }
  | { kind: "failed"; detail: string }
  | { kind: "not_signed_in"; how: string }
  | { kind: "cut" };

export type ExitRef =
  | { kind: "terminated" }
  | { kind: "exited"; status: number | null }
  | { kind: "crashed"; detail: string };

export type EventRef =
  | { kind: "session_opened"; session: SessionRef }
  | { kind: "turn_started"; turn: number; origin: "caller" | "backend" }
  | { kind: "message_partial"; turn: number; text: string }
  | { kind: "message_complete"; turn: number; message: { role: "user" | "assistant"; text: string } }
  | { kind: "tool_call"; turn: number; call: string; name: string; arguments: string }
  | { kind: "tool_result"; turn: number; call: string; output: string; is_error: boolean }
  | { kind: "approval_requested"; turn: number; call: string; invocation: number; rendered: RenderedRef }
  | { kind: "approval_resolved"; turn: number; call: string; invocation: number; allowed: boolean }
  | { kind: "ran_without_asking"; turn: number; call: string; name: string; arguments: string }
  | { kind: "delivery"; id: number; state: string }
  | { kind: "usage"; turn: number; usage: UsageRef }
  | { kind: "diagnostic"; text: string }
  | { kind: "turn_ended"; turn: number; end: TurnEndRef }
  | { kind: "exited"; exit: ExitRef; attachment: number };

export interface ConversationEvent {
  conversation: number;
  event: EventRef;
}

/** One line of the transcript, as the UI shows it. */
export function describe(e: EventRef): string {
  switch (e.kind) {
    case "session_opened":
      return `session ${e.session.value} (${e.session.backend}, ${e.session.account})`;
    case "turn_started":
      return e.origin === "backend" ? `turn ${e.turn} started by the backend` : `turn ${e.turn} started`;
    case "message_partial":
      return e.text;
    case "message_complete":
      return `${e.message.role}: ${e.message.text}`;
    case "tool_call":
      return `tool ${e.name} ${e.arguments}`;
    case "tool_result":
      return `${e.is_error ? "tool error" : "tool result"}: ${e.output}`;
    case "approval_requested":
      return `approval requested — ${e.rendered.title}: ${e.rendered.body}`;
    case "approval_resolved":
      return `approval ${e.allowed ? "allowed" : "refused"} (${e.invocation})`;
    case "ran_without_asking":
      return `ran without asking: ${e.name} ${e.arguments}`;
    case "delivery":
      return `delivery ${e.id} ${e.state}`;
    case "usage":
      return e.usage.kind === "not_reported"
        ? "usage: not reported"
        : `usage: ${e.usage.input_tokens} in / ${e.usage.output_tokens} out` +
            (e.usage.estimated_cost_micros === null
              ? ""
              : `, estimated cost $${(e.usage.estimated_cost_micros / 1_000_000).toFixed(4)} (estimate, not a bill)`);
    case "diagnostic":
      return `diagnostic: ${e.text}`;
    case "turn_ended":
      switch (e.end.kind) {
        case "failed":
          return `turn ${e.turn} failed: ${e.end.detail}`;
        case "not_signed_in":
          return `turn ${e.turn} failed: this account is not signed in — ${e.end.how}`;
        default:
          return `turn ${e.turn} ${e.end.kind}`;
      }
    case "exited":
      return `exited (${e.exit.kind}${
        e.exit.kind === "exited" ? ` ${e.exit.status ?? "?"}` : e.exit.kind === "crashed" ? `: ${e.exit.detail}` : ""
      })`;
  }
}
