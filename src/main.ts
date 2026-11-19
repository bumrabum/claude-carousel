// The carousel window: one slide at a time, "→ next step" in big letters.
// Slides: recent sessions (waiting for you first), "All caught up" when there are none,
// then earlier cards you already opened or marked done.
// While open it flips by itself every few seconds through the recent sessions;
// arrows / wheel / swipe / ← → take over.
// Data and actions come from the Rust side (src-tauri/src/main.rs).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Source = "code" | "cowork" | "cli";

interface Card {
  id: string;
  source: Source;
  sessionId: string;
  title: string;
  project: string;
  cwd: string;
  summary: string;
  activityAt: number;
  waiting: boolean;
  working: boolean;
  nextStep: { stopped: string; next: string } | null;
  summarizing: boolean;
}

interface View {
  cards: Card[];
  history: Card[];
  autoHideSec: number;
  autoFlipSec: number;
  summaryError: string | null;
}

type Slide = { kind: "recent"; card: Card } | { kind: "earlier"; card: Card } | { kind: "empty" };

const SOURCE_LABEL: Record<Source, string> = { code: "Code", cowork: "Cowork", cli: "CLI" };
/** With no cards the window goes away sooner. */
const EMPTY_HIDE_SEC = 6;
/** Auto-flipping stops and the window hides after one round, but never later than this. */
const MAX_ROUND_SEC = 120;

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const el = {
  card: $("card"),
  source: $("source"),
  where: $("where"),
  counter: $("counter"),
  step: $("step"),
  stopped: $("stopped"),
  title: $("title"),
  body: $("body"),
};

let slides: Slide[] = [];
let recentCount = 0;
let index = 0;
let autoHideSec = 15;
let autoFlipSec = 4;
let hideTimer: number | undefined;
let flipTimer: number | undefined;
let hovered = false;
/** You flipped by hand since the window opened: no more auto-flipping. */
let manual = false;
let summaryError: string | null = null;

// ---------- data ----------

async function refresh() {
  const currentKey = slideKey(slides[index]);
  const view = await invoke<View>("get_cards");
  autoHideSec = view.autoHideSec;
  autoFlipSec = view.autoFlipSec;
  summaryError = view.summaryError;
  recentCount = view.cards.length;
  slides = [
    ...view.cards.map((card): Slide => ({ kind: "recent", card })),
    ...(view.cards.length === 0 ? [{ kind: "empty" } as Slide] : []),
    ...view.history.map((card): Slide => ({ kind: "earlier", card })),
  ];
  // Stay on the same slide if it is still there.
  const kept = slides.findIndex((s) => slideKey(s) === currentKey);
  index = kept >= 0 ? kept : Math.min(index, slides.length - 1);
  render();
}

function slideKey(s: Slide | undefined): string {
  if (!s) return "";
  return s.kind === "empty" ? "empty" : `${s.kind}:${s.card.id}`;
}

// ---------- drawing ----------

function render(direction = 0) {
  const slide = slides[index];
  if (!slide) return;
  el.card.classList.toggle("empty", slide.kind === "empty");
  el.card.classList.toggle("earlier", slide.kind === "earlier");
  el.card.classList.toggle("single", slides.length < 2);
  el.counter.textContent = slides.length > 1 ? `${index + 1} / ${slides.length}` : "";

  if (slide.kind === "empty") {
    el.card.dataset.source = "none";
    el.card.classList.remove("is-waiting", "raw", "has-error");
    el.where.textContent = slides.length > 1 ? "← → earlier cards" : "";
    el.step.textContent = "All caught up";
    el.stopped.textContent = "No recent sessions to come back to.";
    el.title.textContent = "";
    el.card.title = "";
  } else {
    drawCard(slide.card, slide.kind === "earlier");
  }

  if (direction !== 0) {
    el.body.classList.remove("slide-left", "slide-right");
    void el.body.offsetWidth; // restart the animation
    el.body.classList.add(direction > 0 ? "slide-left" : "slide-right");
  }
}

function drawCard(card: Card, earlier: boolean) {
  el.card.dataset.source = card.source;
  el.card.classList.toggle("is-waiting", card.waiting && !earlier);
  el.source.textContent = earlier ? "Earlier" : SOURCE_LABEL[card.source];
  el.where.textContent = [card.project, timeAgo(card.activityAt)].filter(Boolean).join(" · ");
  el.title.textContent = card.title;
  el.card.title = card.cwd;

  const step = card.nextStep;
  el.card.classList.toggle("raw", !step);
  el.card.classList.toggle("has-error", !step && !card.summarizing && !!summaryError);
  if (step) {
    el.step.textContent = `→ ${step.next}`;
    el.stopped.textContent = `Stopped: ${step.stopped}`;
  } else if (card.working) {
    el.step.textContent = "Claude is working…";
    el.stopped.textContent = card.summary;
  } else {
    el.step.textContent = card.summary || "Claude is waiting for your reply.";
    el.stopped.textContent = card.summarizing
      ? "Working out the next step…"
      : summaryError
        ? `No next step: ${summaryError}`
        : "";
    el.stopped.title = el.stopped.textContent;
  }
}

function timeAgo(ms: number): string {
  const min = Math.max(1, Math.round((Date.now() - ms) / 60_000));
  if (min < 60) return `${min} min ago`;
  const h = Math.round(min / 60);
  if (h < 48) return `${h} h ago`;
  return `${Math.round(h / 24)} d ago`;
}

function go(step: number) {
  if (slides.length < 2) return;
  index = (index + step + slides.length) % slides.length;
  render(step);
}

/** Flipping by hand: stop auto-flipping, keep the window open while you look. */
function move(step: number) {
  manual = true;
  stopAutoFlip();
  go(step);
  startHideTimer();
}

// ---------- auto-flip and auto-hide ----------

function startAutoFlip() {
  stopAutoFlip();
  if (manual || autoFlipSec <= 0 || recentCount < 2) return;
  flipTimer = window.setInterval(() => {
    if (hovered) return;
    // Go round the recent sessions only, not the earlier cards.
    index = index + 1 < recentCount ? index + 1 : 0;
    render(1);
  }, autoFlipSec * 1000);
}

function stopAutoFlip() {
  window.clearInterval(flipTimer);
}

/** Hide by itself unless the mouse is over the card. */
function startHideTimer() {
  clearTimeout(hideTimer);
  if (hovered) return;
  let seconds = autoHideSec;
  if (recentCount === 0) seconds = EMPTY_HIDE_SEC;
  else if (!manual && autoFlipSec > 0 && recentCount > 1) {
    // Let it go round once.
    seconds = Math.min(MAX_ROUND_SEC, Math.max(autoHideSec, recentCount * autoFlipSec + 1));
  }
  hideTimer = window.setTimeout(hide, seconds * 1000);
}

function hide() {
  clearTimeout(hideTimer);
  stopAutoFlip();
  invoke("hide_window");
}

// ---------- actions ----------

async function act(command: "open_card" | "mark_done" | "snooze") {
  const slide = slides[index];
  if (!slide || slide.kind === "empty") return;
  if (slide.kind === "earlier" && command !== "open_card") return;
  manual = true;
  stopAutoFlip();
  await invoke(command, { id: slide.card.id });
  await refresh();
  startHideTimer();
}

// ---------- wiring ----------

$("prev").onclick = () => move(-1);
$("next").onclick = () => move(1);
$("close").onclick = hide;
$("open").onclick = () => act("open_card");
$("later").onclick = () => act("snooze");
$("done").onclick = () => act("mark_done");
$("retry").onclick = async () => {
  await invoke("retry_summaries");
  await refresh();
};

el.card.addEventListener("mouseenter", () => {
  hovered = true;
  clearTimeout(hideTimer);
});
el.card.addEventListener("mouseleave", () => {
  hovered = false;
  startHideTimer();
});

// Wheel / trackpad swipe flips slides; a short cooldown keeps one swipe = one slide.
let wheelLock = 0;
el.card.addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    const delta = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
    if (Math.abs(delta) < 8 || Date.now() < wheelLock) return;
    wheelLock = Date.now() + 350;
    move(delta > 0 ? 1 : -1);
  },
  { passive: false },
);

window.addEventListener("keydown", (e) => {
  if (e.key === "ArrowRight" || e.key === "ArrowDown") move(1);
  else if (e.key === "ArrowLeft" || e.key === "ArrowUp") move(-1);
  else if (e.key === "Escape") hide();
  else if (e.key === "Enter") act("open_card");
});

listen("cards-changed", refresh);
listen("shown", async () => {
  index = 0;
  manual = false;
  await refresh();
  startAutoFlip();
  startHideTimer();
});

refresh();
