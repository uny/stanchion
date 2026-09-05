import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * The skeleton's whole surface: call the one command the core exposes and show what came
 * back. It proves the IPC boundary works before any real feature depends on it.
 */
export function App() {
  const [version, setVersion] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<string>("core_version")
      .then(setVersion)
      .catch((cause: unknown) => setError(String(cause)));
  }, []);

  return (
    <main>
      <h1>stanchion</h1>
      {error !== null ? (
        <p role="alert">core unreachable: {error}</p>
      ) : (
        <p>core {version ?? "…"}</p>
      )}
    </main>
  );
}
