// intentionally calls a path that does NOT exist on the backend
export async function getElysperHealth(baseURL: string) {
  const res = await fetch(`${baseURL}/elysper/health`, { method: "GET" });
  return res.json();
}
