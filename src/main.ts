// The carousel window: shows one slide at a time, flip with arrows / wheel / swipe.
// Slides: cards waiting for you, then "All caught up" (only when nothing waits),
// then earlier cards you already opened or marked done.
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
}

interface View {
  cards: Card[];
  history: Card[];
  autoHideSec: number;
}

type Slide = { kind: "waiting"; card: Card } | { kind: "earlier"; card: Card } | { kind: "empty" };

const SOURCE_LABEL: Record<Source, string> = { code: "Code", cowork: "Cowork", cli: "CLI" };
/** With nothing waiting the window goes away sooner. */
const EMPTY_HIDE_SEC = 6;

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const el = {
  card: $("card"),
  source: $("source"),
  where: $("where"),
  counter: $("counter"),
  title: $("title"),
  summary: $("summary"),
  body: $("body"),
};

let slides: Slide[] = [];
let waitingCount = 0;
let index = 0;
let autoHideSec = 15;
let hideTimer: number | undefined;
let hovered = false;

// ---------- data ----------

async function refresh() {
  const currentKey = slideKey(slides[index]);
  const view = await invoke<View>("get_cards");
  autoHideSec = view.autoHideSec;
  waitingCount = view.cards.length;
  slides = [
    ...view.cards.map((card): Slide => ({ kind: "waiting", card })),
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
    el.where.textContent = slides.length > 1 ? "← → earlier cards" : "";
    el.title.textContent = "All caught up";
    el.summary.textContent = "No sessions are waiting for your reply.";
    el.card.title = "";
  } else {
    const card = slide.card;
    el.card.dataset.source = card.source;
    el.source.textContent = slide.kind === "earlier" ? "Earlier" : SOURCE_LABEL[card.source];
    el.where.textContent = [card.project, timeAgo(card.activityAt)].filter(Boolean).join(" · ");
    el.title.textContent = card.title;
    el.summary.textContent = card.summary || "Claude is waiting for your reply.";
    el.card.title = card.cwd;
  }

  if (direction !== 0) {
    el.body.classList.remove("slide-left", "slide-right");
    void el.body.offsetWidth; // restart the animation
    el.body.classList.add(direction > 0 ? "slide-left" : "slide-right");
  }
}

function timeAgo(ms: number): string {
  const min = Math.max(1, Math.round((Date.now() - ms) / 60_000));
  if (min < 60) return `${min} min ago`;
  const h = Math.round(min / 60);
  if (h < 48) return `${h} h ago`;
  return `${Math.round(h / 24)} d ago`;
}

function move(step: number) {
  if (slides.length < 2) return;
  index = (index + step + slides.length) % slides.length;
  render(step);
  startHideTimer(); // flipping counts as "still looking"
}

// ---------- actions ----------

async function act(command: "open_card" | "mark_done" | "snooze") {
  const slide = slides[index];
  if (!slide || slide.kind === "empty") return;
  if (slide.kind === "earlier" && command !== "open_card") return;
  await invoke(command, { id: slide.card.id });
  await refresh();
  startHideTimer();
}

function hide() {
  clearTimeout(hideTimer);
  invoke("hide_window");
}

/** Hide by itself unless the mouse is over the card. */
function startHideTimer() {
  clearTimeout(hideTimer);
  const seconds = waitingCount === 0 ? EMPTY_HIDE_SEC : autoHideSec;
  if (!hovered) hideTimer = window.setTimeout(hide, seconds * 1000);
}

// ---------- wiring ----------

$("prev").onclick = () => move(-1);
$("next").onclick = () => move(1);
$("close").onclick = hide;
$("open").onclick = () => act("open_card");
$("later").onclick = () => act("snooze");
$("done").onclick = () => act("mark_done");

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
  await refresh();
  startHideTimer();
});

refresh();
