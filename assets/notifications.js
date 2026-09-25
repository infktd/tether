// The live unread count: the server sends the top bar's bell again (as
// HTML it rendered) whenever the count changes. htmx has no server-sent
// events without an extension; this is all it takes.
(() => {
  if (!window.EventSource) return;
  const source = new EventSource("/notifications/stream");
  source.addEventListener("unread", (event) => {
    const bell = document.getElementById("notification-bell");
    if (bell) bell.outerHTML = event.data;
  });
})();
