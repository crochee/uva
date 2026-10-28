// Fixture: the call shapes the web client uses for the two routes
// that are not plain `fetch('/literal')` calls.
//
// - `FOLLOW_URL` is an arrow-function URL helper: the template body
//   is the path, the argument is the session id.
// - `api.getRaw` fetches a path and hands back the raw `Response`
//   (the session export download does).
//
// The `api` client is declared here (the real one lives in
// `elysper-web/src/api/client.ts`) so the fixture type-checks with
// the rest of the tool: this file is inside the `tsc --noEmit`
// include, and a fixture that does not compile would hide a real
// type error behind a scanner input.
declare const api: {
  getRaw(path: string): Promise<Response>;
};

const FOLLOW_URL = (sessionId: string): string =>
  `/api/v1/sessions/${encodeURIComponent(sessionId)}/follow`;

export async function followSession(sessionId: string): Promise<void> {
  await fetch(FOLLOW_URL(sessionId), { method: 'GET' });
}

export async function exportSession(sessionId: string): Promise<void> {
  await api.getRaw(`/api/v1/sessions/${encodeURIComponent(sessionId)}/export`);
}
