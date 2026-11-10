// splitlane.dev. Two small enhancements; every page works without them.
// Loaded from this origin only, as the site's Content Security Policy
// requires.

// A Copy button beside an install command. The button ships hidden, so a
// browser without script never shows a button that does nothing.
for (const button of document.querySelectorAll("button[data-copy]")) {
  const source = document.getElementById(button.dataset.copy);
  if (!source || !navigator.clipboard) continue;
  button.hidden = false;
  button.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(source.textContent.trim());
      button.textContent = "Copied";
    } catch {
      button.textContent = "Select and copy";
    }
    setTimeout(() => (button.textContent = "Copy"), 1600);
  });
}

// Product videos play while they are on screen, muted, and only for people
// who have not asked for less motion; the rest see the poster and controls.
const still = window.matchMedia("(prefers-reduced-motion: reduce)");
// A video with a clip per theme has a poster per theme too; <source media>
// picks the clip, and the poster attribute holds the light one.
const dark = window.matchMedia("(prefers-color-scheme: dark)");
for (const video of document.querySelectorAll("video[data-poster-dark]")) {
  if (dark.matches) video.poster = video.dataset.posterDark;
}
const videos = document.querySelectorAll("video[data-autoplay]");
if (videos.length && "IntersectionObserver" in window) {
  const seen = new IntersectionObserver(
    (entries) => {
      for (const { target, isIntersecting } of entries) {
        if (isIntersecting && !still.matches) target.play().catch(() => {});
        else target.pause();
      }
    },
    { threshold: 0.4 },
  );
  for (const video of videos) seen.observe(video);
}
