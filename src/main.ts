import "./styles.css";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, isTauri } from "./api";
import type { Category, ClipSummary, ClipQuery, ContentType, Grouping, Settings } from "./types";
import { hydrateIcons } from "./ui/icons";
import { mainWindowShell } from "./ui/main-window";
import { popupShell } from "./ui/popup";
import { relativeTime } from "./ui/time";

const popup = new URLSearchParams(location.search).get("mode") === "popup";
const state: {
  clips: ClipSummary[];
  categories: Category[];
  settings: Settings | null;
  search: string;
  type: ContentType | null;
  category: string | null;
  domain: string | null;
  grouping: Grouping;
  selected: number;
  hasMore: boolean;
} = {
  clips: [],
  categories: [],
  settings: null,
  search: "",
  type: null,
  category: null,
  domain: null,
  grouping: "none",
  selected: 0,
  hasMore: false,
};

/** Tracks whether the invalid-settings warning has already been shown this session. */
let invalidWarningConsumed = false;

const app = document.querySelector<HTMLElement>("#app")!;
const esc = (value: string) =>
  value.replace(/[&<>'"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" }[c]!));

function showDragHintBanner(message: string) {
  let banner = document.querySelector<HTMLElement>("#drag-hint-banner");
  if (!banner) {
    banner = document.createElement("div");
    banner.id = "drag-hint-banner";
    document.body.appendChild(banner);
  }
  banner.textContent = message;
}

function removeDragHintBanner() {
  document.querySelector("#drag-hint-banner")?.remove();
}

function cleanupDragState() {
  document.body.classList.remove("is-dragging-card");
  removeDragHintBanner();
  document.querySelectorAll(".chip.user-chip.drop-over").forEach((el) => el.classList.remove("drop-over"));
}

// ── Toast notifications ───────────────────────────────────────────────────────


function showToast(message: string, type: "error" | "info" = "error") {
  let container = document.querySelector<HTMLElement>("#toast-container");
  if (!container) {
    container = document.createElement("div");
    container.id = "toast-container";
    container.setAttribute("aria-live", "assertive");
    container.style.cssText =
      "position:fixed;bottom:24px;right:24px;z-index:9999;display:flex;flex-direction:column;gap:8px;max-width:360px;";
    document.body.appendChild(container);
  }
  const toast = document.createElement("div");
  toast.className = `toast toast-${type}`;
  toast.textContent = message;
  toast.style.cssText =
    `padding:10px 16px;border-radius:8px;font-size:13px;line-height:1.4;pointer-events:auto;` +
    (type === "error"
      ? "background:#b91c1c;color:#fff;"
      : "background:#1e293b;color:#e2e8f0;");
  container.appendChild(toast);
  window.setTimeout(() => {
    toast.remove();
  }, 4000);
}

// ── Utilities ─────────────────────────────────────────────────────────────────


function query(): ClipQuery {
  return {
    search: state.search || undefined,
    contentType: state.type || undefined,
    categoryId: state.category || undefined,
    domain: state.domain || undefined,
    limit: popup ? 16 : 60,
  };
}

async function refresh() {
  state.clips = await api.list(query());
  state.hasMore = !popup && state.clips.length === 60;
  state.selected = Math.min(state.selected, Math.max(0, state.clips.length - 1));
  renderFilters();
  renderCards();
}

// ── Filters ───────────────────────────────────────────────────────────────────

function filtersMarkup(): string {
  const types: ContentType[] = ["Text", "Links", "Email", "Numbers"];
  const typeLabels: Record<ContentType, string> = { Text: "Текст", Links: "Ссылки", Email: "Почта", Numbers: "Числа" };
  const domains = [...new Set(state.clips.map((c) => c.domain).filter((v): v is string => !!v))].slice(0, 5);
  const allActive = !state.type && !state.category && !state.domain;
  return [
    `<button class="chip ${allActive ? "active" : ""}" data-filter="all">Все</button>`,
    ...types.map((t) => `<button class="chip ${state.type === t ? "active" : ""}" data-type="${t}">${typeLabels[t]}</button>`),
    ...state.categories.map((c) => `<button class="chip user-chip ${state.category === c.id ? "active" : ""}" data-category="${c.id}" style="--tag:${c.color}">${esc(c.name)}</button>`),
    ...domains.map((d) => `<button class="chip ${state.domain === d ? "active" : ""}" data-domain="${esc(d)}">${esc(d)}</button>`),
    `<span class="spacer"></span>`,
    popup ? "" : `<select class="grouping-select" id="grouping" aria-label="Группировка"><option value="none" ${state.grouping === "none" ? "selected" : ""}>Без группировки</option><option value="domain" ${state.grouping === "domain" ? "selected" : ""}>По домену</option><option value="category" ${state.grouping === "category" ? "selected" : ""}>По категории</option><option value="type" ${state.grouping === "type" ? "selected" : ""}>По типу</option></select>`,
  ].join("");
}

function renderFilters() {
  const root = document.querySelector<HTMLElement>("#filters-container");
  if (!root) return;
  root.innerHTML = filtersMarkup();
  const select = root.querySelector<HTMLSelectElement>("#grouping");
  if (select) select.value = state.grouping;
  bindFilters(root);
  hydrateIcons(root);
}

function bindFilters(root: HTMLElement) {
  root.querySelectorAll<HTMLElement>("[data-type]").forEach(
    (el) =>
      (el.onclick = () => {
        state.type = state.type === el.dataset.type ? null : (el.dataset.type as ContentType);
        state.category = state.domain = null;
        refresh();
      })
  );
  root.querySelectorAll<HTMLElement>("[data-category]").forEach((el) => {
    el.onclick = () => {
      state.category = state.category === el.dataset.category ? null : el.dataset.category!;
      state.type = state.domain = null;
      refresh();
    };
    el.ondragover = (e) => {
      e.preventDefault();
      if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
      el.classList.add("drop-over");
    };
    el.ondragleave = () => {
      el.classList.remove("drop-over");
    };
    el.ondrop = (e) => {
      e.preventDefault();
      el.classList.remove("drop-over");
      cleanupDragState();
      const clipId = e.dataTransfer?.getData("text/kitsupin");
      const catId = el.dataset.category;
      if (!clipId || !catId) return;
      const targetCat = state.categories.find((c) => c.id === catId);
      const targetClip = state.clips.find((c) => c.id === clipId);
      const catName = targetCat ? targetCat.name : "";
      if (targetClip && targetClip.categories.some((c) => c.id === catId)) {
        showToast(`Карточка уже в категории «${catName}»`, "info");
        return;
      }
      api.assign(clipId, catId)
        .then(() => {
          showToast(`Добавлено в категорию «${catName}»`, "info");
          reload();
        })
        .catch(() => showToast("Не удалось назначить категорию"));
    };
  });

  root.querySelectorAll<HTMLElement>("[data-domain]").forEach(
    (el) =>
      (el.onclick = () => {
        state.domain = state.domain === el.dataset.domain ? null : el.dataset.domain!;
        state.type = state.category = null;
        refresh();
      })
  );
  const allBtn = root.querySelector<HTMLElement>("[data-filter=all]");
  if (allBtn) {
    allBtn.onclick = () => {
      state.type = state.category = state.domain = null;
      refresh();
    };
  }
  root.querySelector("[data-action=new-category]")?.addEventListener("click", showCategoryModal);
}

// ── Shell ─────────────────────────────────────────────────────────────────────

function renderShell() {
  app.className = popup ? "popup-shell" : "main-shell";
  app.innerHTML = popup ? popupShell() : mainWindowShell();
  bindShell();
  hydrateIcons(app);
}

function showScreen(id: string) {
  document.querySelectorAll<HTMLElement>(".screen").forEach((s) => {
    s.classList.toggle("active", s.id === id);
  });
  document.querySelectorAll<HTMLElement>(".sidebar .navbtn[data-screen]").forEach((b) => {
    const active = b.dataset.screen === id;
    b.classList.toggle("active", active);
    b.setAttribute("aria-current", active ? "page" : "false");
  });
  // When switching to settings screen, hydrate its content
  if (id === "settings") {
    renderSettingsScreen();
  } else {
    draftSettings = null;
    settingsDirty = false;
  }
  // When switching to categories screen, render list
  if (id === "categories") renderCategoriesScreen();
}

function bindShell() {
  const input = document.querySelector<HTMLInputElement>("#search");
  let timer = 0;
  input?.addEventListener("input", () => {
    state.search = input!.value;
    clearTimeout(timer);
    timer = window.setTimeout(refresh, 120);
  });
  if (popup && input) setTimeout(() => input.focus(), 20);

  // Sidebar navigation
  document.querySelectorAll<HTMLElement>(".sidebar .navbtn[data-screen]").forEach((b) => {
    b.addEventListener("click", () => showScreen(b.dataset.screen!));
  });

  // Settings button in toolbar navigates to settings screen
  document.querySelector("[data-action=settings]")?.addEventListener("click", () => showScreen("settings"));

  // Pause/resume button
  const pauseBtn = document.querySelector<HTMLButtonElement>("#btn-pause");
  pauseBtn?.addEventListener("click", () => {
    const s = state.settings;
    if (!s) return;
    const paused = !s.paused;
    api.saveSettings({ ...s, paused }).then(() => {
      state.settings = { ...s, paused };
      updateRecordingStatus(paused);
      if (pauseBtn) pauseBtn.querySelector("span")!.textContent = paused ? "Продолжить" : "Пауза";
      showToast(paused ? "Запись приостановлена" : "Запись возобновлена", "info");
    }).catch(() => showToast("Не удалось изменить статус записи"));
  });

  // Window buttons (Tauri)
  if (isTauri) {
    document.querySelector("#btn-minimize")?.addEventListener("click", () => getCurrentWindow().minimize());
    document.querySelector("#btn-close")?.addEventListener("click", () => getCurrentWindow().close());
  }

  // New category button on categories screen
  document.querySelector("#btn-new-category")?.addEventListener("click", showCategoryModal);

  // Grouping select (delegated — filters-container is re-rendered)
  document.querySelector("#filters-container")?.addEventListener("change", (e) => {
    const sel = e.target as HTMLSelectElement;
    if (sel.id === "grouping") {
      state.grouping = sel.value as Grouping;
      renderCards();
    }
  });
}

function updateRecordingStatus(paused: boolean) {
  const dot = document.querySelector<HTMLElement>("#status-dot");
  const text = document.querySelector<HTMLElement>("#status-text");
  dot?.classList.toggle("paused", paused);
  if (text) text.textContent = paused ? "Запись приостановлена" : "История записывается";
  const pauseBtn = document.querySelector<HTMLButtonElement>("#btn-pause");
  if (pauseBtn) {
    const span = pauseBtn.querySelector("span");
    if (span) span.textContent = paused ? "Продолжить" : "Пауза";
    pauseBtn.title = paused ? "Возобновить запись" : "Приостановить запись";
  }
}

// ── Card rendering ────────────────────────────────────────────────────────────

const TYPE_LABELS: Record<string, string> = { Text: "Текст", Links: "Ссылка", Email: "Почта", Numbers: "Число" };

function clipCard(c: ClipSummary, index: number) {
  const isTruncated = c.isTruncated;
  const typeLabel = TYPE_LABELS[c.contentType] ?? c.contentType;

  if (popup) {
    // Compact popup style — popitem
    const metaParts: string[] = [typeLabel];
    if (c.domain) metaParts.push(esc(c.domain));
    if (c.pageTitle) metaParts.push(esc(c.pageTitle));
    const metaLabel = metaParts.join(" · ");
    const isSelected = index === state.selected;
    return `<button class="popitem ${isSelected ? "selected" : ""}" tabindex="0" data-clip="${c.id}" id="clip-item-${index}" role="option" aria-selected="${isSelected}" aria-label="Скопировать фрагмент ${index + 1}: ${esc(c.preview)}">
      <span class="popnum">${index + 1 <= 9 ? index + 1 : "·"}</span>
      <span><p>${esc(c.preview)}${isTruncated ? "…" : ""}</p><small>${metaLabel}</small></span>
      <span class="kbd">Enter</span>
    </button>`;
  }

  // Main window — list item style matching prototype
  const categories = c.categories
    .map((x) => `<button class="tag user-tag" style="--tag:${x.color}" data-unassign="${x.id}" title="Убрать категорию">${esc(x.name)} ×</button>`)
    .join("");
  const detailsButton = isTruncated
    ? `<button class="iconbtn" data-action="details" aria-label="Полный текст" title="Полный текст"><i data-lucide="eye"></i></button>`
    : "";
  const pinIcon = c.pinned
    ? `<button class="iconbtn" data-action="pin" aria-label="Открепить"><i data-lucide="pin-off"></i></button>`
    : `<button class="iconbtn" data-action="pin" aria-label="Закрепить"><i data-lucide="pin"></i></button>`;

  const dragHandleHtml = state.categories.length > 0
    ? `<button class="drag-handle" data-clip-drag="${c.id}" draggable="true" title="Перетащить в категорию" aria-label="Перетащить в категорию"><i data-lucide="grip-vertical"></i></button>`
    : "";

  const headerSource = [
    c.domain ? `<span>${esc(c.domain)}</span>` : "",
    c.pageTitle ? `<span>${esc(c.pageTitle)}</span>` : ""
  ].filter(Boolean).join("<span>·</span>");

  return `<article class="clip ${c.pinned ? "pinned" : ""}" data-clip="${c.id}">
    ${dragHandleHtml}
    <div class="clip-main" tabindex="0" role="button" aria-label="Скопировать фрагмент">
      <div class="clip-top">
        <strong>${esc(typeLabel)}</strong>
        ${headerSource ? `<span>·</span>${headerSource}` : ""}
      </div>
      <div class="clip-text">${esc(c.preview)}${isTruncated ? "…" : ""}</div>
      <div class="clip-meta">
        ${categories}
        <span>${c.copyCount > 1 ? `скопировано ${c.copyCount} раз` : "одна копия"}${isTruncated ? ` · ${c.contentLength} симв.` : ""}</span>
        <span>· ${relativeTime(c.lastCopiedAt)}</span>
      </div>
    </div>
    <div class="clip-actions">
      ${detailsButton}
      ${pinIcon}
      <button class="iconbtn" data-action="assign-cat" aria-label="Категория" title="Категория"><i data-lucide="tag"></i></button>
      <button class="iconbtn" data-action="delete" aria-label="Удалить"><i data-lucide="trash-2"></i></button>
    </div>
  </article>`;
}


function groups(): [string, ClipSummary[]][] {
  if (state.grouping === "none") return [["", state.clips]];
  if (state.grouping === "type")
    return (["Text", "Links", "Email", "Numbers"] as ContentType[]).map((t) => [t, state.clips.filter((c) => c.contentType === t)]);
  if (state.grouping === "domain") {
    const map = new Map<string, ClipSummary[]>();
    const noSource: ClipSummary[] = [];
    for (const c of state.clips) {
      if (c.domain) {
        const list = map.get(c.domain) || [];
        list.push(c);
        map.set(c.domain, list);
      } else {
        noSource.push(c);
      }
    }
    const result: [string, ClipSummary[]][] = Array.from(map.entries());
    if (noSource.length) {
      result.push(["Без источника", noSource]);
    }
    return result;
  }
  return [
    ...state.categories.map((cat) => [cat.name, state.clips.filter((c) => c.categories.some((x) => x.id === cat.id))] as [string, ClipSummary[]]),
    ["Без категории", state.clips.filter((c) => !c.categories.length)],
  ];
}

function renderCards() {
  const root = document.querySelector<HTMLElement>("#cards");
  if (!root) return;
  if (!state.clips.length) {
    const hasFilter = !!(state.search || state.type || state.category || state.domain);
    root.innerHTML = `<div class="empty">
      <svg class="fox" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="M4 5.5 8.2 8 12 6.5 15.8 8 20 5.5l-1.2 8.2C18.3 17.4 15.5 20 12 21c-3.5-1-6.3-3.6-6.8-7.3L4 5.5Z"/></svg>
      <h2>${hasFilter ? "Лиса ничего не нашла" : "История пока пуста"}</h2>
      <p>${hasFilter ? "Попробуйте другой запрос или сбросьте фильтры." : "Скопируйте текст через Ctrl+C — он появится здесь."}</p>
    </div>`;
    return;
  }
  if (popup) {
    root.innerHTML = state.clips.map((c, i) => clipCard(c, i)).join("");
  } else {
    const grouped = groups().filter(([, clips]) => clips.length);
    root.innerHTML = grouped
      .map(([name, clips]) => [
        name ? `<div class="sectionhead">${esc(name)}</div>` : "",
        clips.map((c, i) => clipCard(c, i)).join(""),
      ].join(""))
      .join("") + (state.hasMore ? '<button class="load-more" data-load-more>Показать ещё</button>' : "");
  }
  bindCards(root);
  root.querySelector("[data-load-more]")?.addEventListener("click", loadMore);
  hydrateIcons(root);
  if (popup) {
    const selectedEl = root.querySelector<HTMLElement>(".popitem.selected");
    selectedEl?.scrollIntoView({ block: "nearest" });
    const searchInput = document.querySelector<HTMLInputElement>("#search");
    if (searchInput) {
      if (state.clips.length > 0 && state.selected >= 0) {
        searchInput.setAttribute("aria-activedescendant", `clip-item-${state.selected}`);
      } else {
        searchInput.removeAttribute("aria-activedescendant");
      }
    }
    const announcer = document.querySelector<HTMLElement>("#sr-announcer");
    const activeClip = state.clips[state.selected];
    if (announcer && activeClip) {
      announcer.textContent = `Выбран элемент ${state.selected + 1} из ${state.clips.length}: ${activeClip.preview}`;
    }
  }
}

async function loadMore() {
  const next = await api.list({ ...query(), offset: state.clips.length });
  state.clips.push(...next);
  state.hasMore = next.length === 60;
  renderCards();
}

function bindCards(root: HTMLElement) {
  root.querySelectorAll<HTMLElement>("[data-clip]").forEach((card) => {
    const clip = state.clips.find((c) => c.id === card.dataset.clip)!;

    // Main action: copy on click on clip-main or on popitem
    const mainBtn = card.classList.contains("popitem") ? card : card.querySelector<HTMLElement>(".clip-main");
    const handleCopy = async (e: Event) => {
      if (e.target instanceof Element) {
        const closestInteractive = e.target.closest(".tag, .clip-actions, button, input, a");
        if (closestInteractive && closestInteractive !== mainBtn) {
          return;
        }
      }
      try {
        let content: string | undefined;
        if (!isTauri) content = await api.getClipContent(clip.id);
        await api.copy(clip.id, popup, content);
      } catch {
        showToast("Не удалось скопировать");
      }
    };

    mainBtn?.addEventListener("click", handleCopy);
    mainBtn?.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        if (e.target instanceof Element) {
          const closestInteractive = e.target.closest(".tag, .clip-actions, button, input, a");
          if (closestInteractive && closestInteractive !== mainBtn) {
            return;
          }
        }
        e.preventDefault();
        handleCopy(e);
      }
    });

    const dragBtn = card.querySelector<HTMLElement>("[data-clip-drag]");
    if (dragBtn) {
      dragBtn.ondragstart = (e) => {
        e.dataTransfer?.setData("text/kitsupin", clip.id);
        if (e.dataTransfer) e.dataTransfer.effectAllowed = "copy";
        document.body.classList.add("is-dragging-card");
        showDragHintBanner("Перетащите карточку на пользовательскую категорию");
      };
      dragBtn.ondragend = () => {
        cleanupDragState();
      };
      dragBtn.onclick = (e) => {
        e.stopPropagation();
        showAssignCategoryModal(clip);
      };
      dragBtn.onkeydown = (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          e.stopPropagation();
          showAssignCategoryModal(clip);
        }
      };
    }


    card.querySelector<HTMLElement>("[data-action=details]")?.addEventListener("click", (e) => {
      e.stopPropagation();
      showClipDetailsModal(clip.id);
    });

    card.querySelector<HTMLElement>("[data-action=pin]")?.addEventListener("click", async (e) => {
      e.stopPropagation();
      try {
        await api.pin(clip.id, !clip.pinned);
        await reload();
      } catch {
        showToast(clip.pinned ? "Не удалось открепить" : "Не удалось закрепить");
      }
    });

    card.querySelector<HTMLElement>("[data-action=assign-cat]")?.addEventListener("click", (e) => {
      e.stopPropagation();
      showAssignCategoryModal(clip);
    });

    // 6.7: confirm before deleting a pinned clip.
    card.querySelector<HTMLElement>("[data-action=delete]")?.addEventListener("click", async (e) => {
      e.stopPropagation();
      if (clip.pinned) {
        const confirmed = await confirmModal(
          "Удалить закреплённый фрагмент?",
          "Фрагмент закреплён. Удалить его из истории?"
        );
        if (!confirmed) return;
      }
      try {
        await api.remove(clip.id);
        await reload();
      } catch {
        showToast("Не удалось удалить");
      }
    });

    card.querySelectorAll<HTMLElement>("[data-unassign]").forEach(
      (x) =>
        (x.onclick = async (e) => {
          e.stopPropagation();
          try {
            await api.unassign(clip.id, x.dataset.unassign!);
            await reload();
          } catch {
            showToast("Не удалось убрать категорию");
          }
        })
    );
  });
}

// ── Modals ────────────────────────────────────────────────────────────────────

function modal(title: string, body: string): HTMLElement | null {
  const root = document.querySelector<HTMLElement>("#modal-root");
  if (!root) return null;
  root.innerHTML = `<div class="modal-backdrop"><section class="modal" role="dialog" aria-modal="true"><header><h2>${title}</h2><button data-close aria-label="Закрыть"><i data-lucide="x"></i></button></header>${body}</section></div>`;
  root.querySelector("[data-close]")?.addEventListener("click", () => (root.innerHTML = ""));
  hydrateIcons(root);
  return root;
}

/** Show a confirmation dialog using our own modal system.
 *  Returns a promise that resolves to true (confirmed) or false (cancelled). */
function confirmModal(title: string, message: string): Promise<boolean> {
  return new Promise((resolve) => {
    const root = document.querySelector<HTMLElement>("#modal-root");
    if (!root) { resolve(false); return; }
    root.innerHTML = `<div class="modal-backdrop"><section class="modal" role="dialog" aria-modal="true"><header><h2>${esc(title)}</h2></header><p style="padding:12px 0">${esc(message)}</p><div class="modal-actions" style="display:flex;gap:8px;justify-content:flex-end"><button data-cancel>Отмена</button><button class="danger" data-confirm>Удалить</button></div></section></div>`;
    hydrateIcons(root);
    root.querySelector("[data-cancel]")?.addEventListener("click", () => { root.innerHTML = ""; resolve(false); });
    root.querySelector("[data-confirm]")?.addEventListener("click", () => { root.innerHTML = ""; resolve(true); });
  });
}

async function showClipDetailsModal(clipId: string) {
  const clip = state.clips.find((c) => c.id === clipId);
  if (!clip) return;
  const content = await api.getClipContent(clipId);
  const root = modal(
    "Полный текст",
    `<div style="padding:16px"><div style="display:flex;gap:16px;font-size:12px;color:var(--muted);margin-bottom:12px">
      <span>Тип: <b>${clip.contentType}</b></span>
      <span>Длина: <b>${clip.contentLength} символов</b></span>
      ${clip.domain ? `<span>Источник: <b>${esc(clip.domain)}</b></span>` : ""}
    </div>
    <div class="fulltext">${esc(content)}</div>
    <div class="modal-actions">
      <button class="btn primary" data-action="copy-details">Скопировать в буфер</button>
    </div></div>`
  );
  if (!root) return;
  root.querySelector("[data-action=copy-details]")?.addEventListener("click", async () => {
    try {
      await api.copy(clipId, popup, content);
      root.innerHTML = "";
    } catch {
      showToast("Не удалось скопировать");
    }
  });
}

// ── Categories screen ─────────────────────────────────────────────────────────

function renderCategoriesScreen() {
  const list = document.querySelector<HTMLElement>("#category-list");
  if (!list) return;
  if (!state.categories.length) {
    list.innerHTML = `<div class="empty" style="padding:32px"><p>Категорий пока нет.</p></div>`;
    return;
  }
  list.innerHTML = state.categories
    .map(
      (c) => `<div class="cat-row">
        <i class="swatch" style="--cat-color:${c.color}"></i>
        <strong>${esc(c.name)}</strong>
        <button class="iconbtn" data-edit-cat="${c.id}" aria-label="Изменить категорию ${esc(c.name)}">•••</button>
      </div>`
    )
    .join("");
  list.querySelectorAll<HTMLElement>("[data-edit-cat]").forEach((btn) => {
    btn.addEventListener("click", () => showCategoryModal());
  });
}

function showCategoryModal() {
  const existing = state.categories
    .map(
      (c) =>
        `<form class="category-edit" data-edit="${c.id}"><input name="name" maxlength="60" required value="${esc(
          c.name
        )}"/><input name="color" type="color" value="${c.color}"/><button type="submit">Сохранить</button><button type="button" data-delete="${
          c.id
        }"><i data-lucide="trash-2"></i></button></form>`
    )
    .join("");
  const root = modal(
    "Категории",
    `<div class="modal-category-list" style="padding:0 16px">${
      existing || `<p class="notice" style="margin:12px 0">Пользовательских категорий пока нет.</p>`
    }</div><form id="category-form" style="padding:16px;border-top:1px solid var(--border);display:grid;gap:12px"><label style="display:grid;gap:6px;font-size:12px;font-weight:600">Новая категория<input name="name" maxlength="60" required placeholder="Например, Japanese" style="height:38px;border:1px solid var(--border);border-radius:8px;background:var(--bg);padding:0 10px"/></label><label style="display:grid;gap:6px;font-size:12px;font-weight:600">Цвет<input name="color" type="color" value="#f2a65a" style="width:48px;height:32px;border:1px solid var(--border);border-radius:6px;padding:2px;cursor:pointer"/></label><button class="primary" type="submit" style="justify-self:start">Создать категорию</button></form>`
  );
  if (!root) return;
  root.querySelector<HTMLFormElement>("#category-form")?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const fd = new FormData(e.target as HTMLFormElement);
    try {
      await api.createCategory(String(fd.get("name")), String(fd.get("color")));
      root.innerHTML = "";
      await reload();
    } catch {
      showToast("Не удалось создать категорию");
    }
  });
  root.querySelectorAll<HTMLFormElement>("[data-edit]").forEach((form) =>
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const fd = new FormData(form);
      try {
        await api.updateCategory(form.dataset.edit!, String(fd.get("name")), String(fd.get("color")));
        root.innerHTML = "";
        await reload();
      } catch {
        showToast("Не удалось сохранить категорию");
      }
    })
  );
  root.querySelectorAll<HTMLElement>("[data-delete]").forEach(
    (button) =>
      (button.onclick = async () => {
        const confirmed = await confirmModal(
          "Удалить категорию?",
          "Категория будет удалена из всех фрагментов. Продолжить?"
        );
        if (!confirmed) {
          showCategoryModal();
          return;
        }
        try {
          await api.deleteCategory(button.dataset.delete!);
          root.innerHTML = "";
          await reload();
        } catch {
          showToast("Не удалось удалить категорию");
        }
      })
  );
}

function showAssignCategoryModal(clip: ClipSummary) {
  const root = document.querySelector<HTMLElement>("#modal-root");
  if (!root) return;

  const renderContent = () => {
    const assignedIds = new Set(clip.categories.map((c) => c.id));
    const categoriesHtml = state.categories.length
      ? state.categories
          .map((cat) => {
            const isAssigned = assignedIds.has(cat.id);
            return `<button type="button" class="btn ${isAssigned ? "primary" : ""}" data-toggle-cat="${cat.id}" style="display:flex;align-items:center;justify-content:space-between;width:100%;margin-bottom:8px;padding:8px 12px;border-radius:8px;text-align:left;">
              <span style="display:flex;align-items:center;gap:8px;">
                <i class="swatch" style="--cat-color:${cat.color}"></i>
                <span>${esc(cat.name)}</span>
              </span>
              <span>${isAssigned ? "✓ Назначено" : "+ Назначить"}</span>
            </button>`;
          })
          .join("")
      : `<p class="notice" style="margin:12px 0">Пользовательских категорий пока нет.</p>`;

    root.innerHTML = `<div class="modal-backdrop"><section class="modal" role="dialog" aria-modal="true">
      <header>
        <h2>Назначить категорию</h2>
        <button data-close aria-label="Закрыть"><i data-lucide="x"></i></button>
      </header>
      <div style="padding:16px">
        <div style="margin-bottom:12px;font-size:13px;color:var(--muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;">
          Фрагмент: <strong>${esc(clip.preview.slice(0, 40))}</strong>
        </div>
        <div class="assign-category-list">${categoriesHtml}</div>
        <div style="margin-top:16px;padding-top:12px;border-top:1px solid var(--border);display:flex;justify-content:flex-end;">
          <button type="button" class="btn" id="btn-manage-cats-modal" style="font-size:12px;">Управление категориями</button>
        </div>
      </div>
    </section></div>`;

    hydrateIcons(root);
    root.querySelector("[data-close]")?.addEventListener("click", () => (root.innerHTML = ""));
    root.querySelector("#btn-manage-cats-modal")?.addEventListener("click", () => {
      showCategoryModal();
    });

    root.querySelectorAll<HTMLElement>("[data-toggle-cat]").forEach((btn) => {
      btn.onclick = async () => {
        const catId = btn.dataset.toggleCat!;
        const isAssigned = assignedIds.has(catId);
        try {
          if (isAssigned) {
            await api.unassign(clip.id, catId);
            clip.categories = clip.categories.filter((c) => c.id !== catId);
          } else {
            await api.assign(clip.id, catId);
            const targetCat = state.categories.find((c) => c.id === catId);
            if (targetCat && !clip.categories.some((c) => c.id === catId)) {
              clip.categories.push(targetCat);
            }
          }
          await reload();
          const updatedClip = state.clips.find((c) => c.id === clip.id);
          if (updatedClip) clip = updatedClip;
          renderContent();
        } catch {
          showToast(isAssigned ? "Не удалось убрать категорию" : "Не удалось назначить категорию");
        }
      };
    });
  };

  renderContent();
}

async function renderIntegrationView(container: HTMLElement) {
  container.innerHTML = `<div class="notice" style="margin-bottom:12px;">Загрузка диагностики...</div>`;
  try {
    const status = await api.getIntegrationStatus();
    const handshakeText = status.lastExtensionHandshakeAt ? relativeTime(status.lastExtensionHandshakeAt) : "нет данных";
    const copyMetaText = status.lastBrowserCopyMetadataAt ? relativeTime(status.lastBrowserCopyMetadataAt) : "нет данных";

    let extStatusLabel = "Не подключено";
    let extStatusClass = "status-warn";
    if (status.nativeMessagingConnected) {
      extStatusLabel = "✓ Подключено (Связь активна)";
      extStatusClass = "status-ok";
    } else if (status.nativeMessagingConfigured) {
      extStatusLabel = "ℹ Установлено, но связь ещё не подтверждена";
      extStatusClass = "status-info";
    } else if (!status.nativeSocketAvailable) {
      extStatusLabel = "✗ KitsuPin недоступен";
      extStatusClass = "status-err";
    }

    container.innerHTML = `
      <div class="integration-view">
        <div class="integration-actions" style="display:flex;justify-content:space-between;align-items:center;margin-bottom:12px;">
          <b style="font-size:13px;">Диагностика интеграции</b>
          <button type="button" class="secondary" id="btn-refresh-diagnostics" style="font-size:11px;padding:4px 8px;">
            <i data-lucide="refresh-cw"></i> Обновить статус
          </button>
        </div>

        <div class="diag-group">
          <h4>Система</h4>
          <div class="diag-item"><span>Linux OS</span>${status.isLinux ? '<span class="status-ok">✓ Linux</span>' : '<span class="status-err">✗ Не Linux</span>'}</div>
          <div class="diag-item"><span>Рабочее окружение</span><span>${status.desktopEnvironment ? esc(status.desktopEnvironment) : 'Не определено'}</span></div>
          <div class="diag-item"><span>Тип сеанса</span>${status.isSupportedX11 ? '<span class="status-ok">✓ X11</span>' : '<span class="status-warn">⚠ Wayland (рекомендуется X11)</span>'}</div>
        </div>

        <div class="diag-group">
          <h4>Приложение</h4>
          <div class="diag-item"><span>Native host установлен</span>${status.nativeHostBinaryExists && status.nativeHostExecutable ? '<span class="status-ok">✓ Исполняемый файл найден</span>' : '<span class="status-err">✗ Отсутствует или нет прав +x</span>'}</div>
          <div class="diag-item"><span>Native socket</span>${status.nativeSocketAvailable ? '<span class="status-ok">✓ Работает</span>' : '<span class="status-err">✗ Недоступен</span>'}</div>
          <div class="diag-item"><span>Автозапуск</span>${status.autostartEnabled ? '<span class="status-ok">✓ Включён</span>' : '<span class="status-info">Отключён</span>'}</div>
          <div class="diag-item"><span>Горячая клавиша</span>${status.shortcutRegistered === true ? '<span class="status-ok">✓ Зарегистрирована</span>' : status.shortcutRegistered === false ? '<span class="status-warn">⚠ Не доступна</span>' : '<span class="status-info">Неизвестно</span>'}</div>
        </div>

        <div class="diag-group">
          <h4>Chrome / Chromium и расширение</h4>
          <div class="diag-item"><span>Браузер</span>${status.chromeDetected ? '<span class="status-ok">✓ Обнаружен</span>' : '<span class="status-warn">⚠ Не найден</span>'}</div>
          <div class="diag-item"><span>Chrome Manifest</span>${status.chromeManifestValid ? `<span class="status-ok">✓ ${esc(status.extensionId || '')}</span>` : '<span class="status-warn">⚠ Не настроен</span>'}</div>
          <div class="diag-item"><span>Chromium Manifest</span>${status.chromiumManifestValid ? `<span class="status-ok">✓ ${esc(status.extensionId || '')}</span>` : '<span class="status-warn">⚠ Не настроен</span>'}</div>
          <div class="diag-item"><span>Native Messaging</span><span class="${extStatusClass}">${extStatusLabel}</span></div>
          <div class="diag-item"><span>Последний handshake</span><span>${handshakeText}</span></div>
          <div class="diag-item"><span>Последняя передача метаданных</span><span>${copyMetaText}</span></div>
        </div>


        ${status.problems.map(p => `
          <div class="problem-card ${p.severity}">
            <h5>${esc(p.title)}</h5>
            <p>${esc(p.description)}</p>
          </div>
        `).join('')}

        <div style="margin-top:16px;background:#f8fafc;border:1px solid #e2e8f0;border-radius:6px;padding:12px;">
          <h4 style="margin:0 0 8px 0;font-size:11px;text-transform:uppercase;letter-spacing:.05em;color:var(--muted);">Настройка Chrome-расширения (Alpha / Unpacked)</h4>
          <p style="margin:0 0 10px 0;font-size:11px;color:var(--muted);line-height:1.4;">
            Если расширение ещё не установлено из Chrome Web Store, вы можете загрузить распакованную папку расширения:
          </p>
          <div style="display:flex;gap:8px;margin-bottom:12px;flex-wrap:wrap;">
            <button type="button" id="btn-open-ext-page" style="font-size:11px;padding:6px 10px;">Открыть chrome://extensions</button>
            <button type="button" id="btn-open-ext-dir" style="font-size:11px;padding:6px 10px;">Открыть папку расширения</button>
          </div>
          <div class="id-input-group">
            <label style="font-size:11px;font-weight:700;display:block;margin-bottom:4px;">ID расширения (32 символа a–p):</label>
            <div style="display:flex;gap:8px;">
              <input type="text" id="input-ext-id" placeholder="например, abcdefghijklmnopabcdefghijklmnop" value="${esc(status.extensionId || '')}" style="font-family:monospace;font-size:12px;flex:1;padding:6px 8px;" />
              <button type="button" class="primary" id="btn-save-ext-id" style="font-size:11px;padding:6px 10px;white-space:nowrap;">Сохранить ID</button>
            </div>
          </div>
        </div>
      </div>
    `;
    hydrateIcons(container);

    container.querySelector("#btn-refresh-diagnostics")?.addEventListener("click", () => renderIntegrationView(container));
    container.querySelector("#btn-open-ext-page")?.addEventListener("click", async () => {
      try {
        await api.openChromeExtensionsPage();
        showToast("Открываем chrome://extensions", "info");
      } catch {
        showToast("Откройте chrome://extensions вручную в браузере", "info");
      }
    });
    container.querySelector("#btn-open-ext-dir")?.addEventListener("click", async () => {
      try {
        const path = await api.openExtensionDir();
        showToast(`Папка расширения: ${path}`, "info");
      } catch (e) {
        showToast(typeof e === "string" ? e : "Не удалось открыть папку расширения");
      }
    });
    container.querySelector("#btn-save-ext-id")?.addEventListener("click", async () => {
      const input = container.querySelector<HTMLInputElement>("#input-ext-id");
      const id = input?.value.trim() || "";
      if (!/^[a-p]{32}$/.test(id)) {
        showToast("ID должен состоять из ровно 32 символов a–p");
        return;
      }
      try {
        await api.configureExtensionId(id);
        showToast("Native Messaging manifest успешно обновлён", "info");
        await renderIntegrationView(container);
      } catch (e) {
        showToast(typeof e === "string" ? e : "Не удалось настроить ID расширения");
      }
    });
  } catch (e) {
    container.innerHTML = `<div class="notice" style="border-left-color:var(--coral);background:#fae2dc;color:#92463a;padding:12px;">Не удалось выполнить диагностику: ${esc(String(e))}</div>`;
  }
}

function showSettings() {
  // In the new layout, settings is a screen — just navigate to it
  showScreen("settings");
}

let draftSettings: Settings | null = null;
let settingsDirty = false;

function renderSettingsScreen() {
  if (!draftSettings && state.settings) {
    draftSettings = { ...state.settings };
  }
  const s = draftSettings || state.settings;
  const body = document.querySelector<HTMLElement>("#settings-body");
  if (!body || !s) return;

  // Settings nav tabs
  document.querySelectorAll<HTMLElement>(".settings-nav .navbtn[data-tab]").forEach((btn) => {
    btn.onclick = () => {
      document.querySelectorAll<HTMLElement>(".settings-nav .navbtn").forEach((b) => b.classList.remove("active"));
      btn.classList.add("active");
      if (btn.dataset.tab === "general") renderGeneralTab(body);
      else if (btn.dataset.tab === "integration") renderIntegrationView(body);
      else if (btn.dataset.tab === "about") renderAboutTab(body);
    };
  });

  // Default: show general tab
  document.querySelectorAll<HTMLElement>(".settings-nav .navbtn").forEach((b) => b.classList.remove("active"));
  document.querySelector<HTMLElement>(".settings-nav .navbtn[data-tab='general']")?.classList.add("active");
  renderGeneralTab(body);
}

function renderAboutTab(body: HTMLElement) {
  body.innerHTML = `
    <div class="panel">
      <div class="empty" style="padding: 20px 0;">
        <svg class="fox" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
          <path d="M4 5.5 8.2 8 12 6.5 15.8 8 20 5.5l-1.2 8.2C18.3 17.4 15.5 20 12 21c-3.5-1-6.3-3.6-6.8-7.3L4 5.5Z"/>
          <path d="m8.5 13 3.5 2.5 3.5-2.5M12 15.5V21"/>
        </svg>
        <h1>KitsuPin</h1>
        <p>Локальная история буфера обмена для KDE Plasma и X11.</p>
        <div class="kbd" style="display:inline-block">Версия 0.1.1 · Tauri 2</div>
        <p style="margin-top:12px">Данные хранятся только на этом устройстве.<br>Без аккаунтов, облака и телеметрии.</p>
      </div>
    </div>
  `;
}

function renderGeneralTab(body: HTMLElement) {
  if (!draftSettings && state.settings) {
    draftSettings = { ...state.settings };
  }
  const s = draftSettings || state.settings;
  if (!s) return;
  body.innerHTML = `
    <div class="panel">
      <h2>Основные</h2>
      <div class="setting-row">
        <div><strong>Запись истории</strong><p>Отслеживать обычный X11 Clipboard</p></div>
        <button class="switch ${s.paused ? "" : "on"}" role="switch" type="button" tabindex="0" id="sw-recording" aria-label="Запись" aria-checked="${!s.paused}"></button>
      </div>
      <div class="setting-row">
        <div><strong>Автозапуск</strong><p>Запускать после входа в KDE</p></div>
        <button class="switch ${s.autostart ? "on" : ""}" role="switch" type="button" tabindex="0" id="sw-autostart" aria-label="Автозапуск" aria-checked="${s.autostart}"></button>
      </div>
      <div class="setting-row">
        <div><strong>Горячая клавиша</strong><p>Открывает компактную историю поверх окон.</p></div>
        <input id="input-shortcut" style="width:120px;height:32px;border:1px solid var(--border);border-radius:7px;background:var(--bg);padding:0 8px;font-size:12px" value="${esc(s.shortcut)}"/>
      </div>
      <div class="setting-row">
        <div><strong>Автоочистка (дней)</strong><p>Закреплённые фрагменты сохраняются.</p></div>
        <input id="input-retention" type="number" min="1" max="3650" style="width:80px;height:32px;border:1px solid var(--border);border-radius:7px;background:var(--bg);padding:0 8px;font-size:12px" value="${s.retentionDays}"/>
      </div>
      <div class="notice" style="margin-top:16px">Исключение приложений подготовлено архитектурно, но отключено в MVP: X11 не сообщает надёжно источник Clipboard во всех случаях.</div>
      <div style="display:flex;gap:8px;margin-top:20px">
        <button class="btn primary" id="btn-save-settings">Сохранить</button>
        <button class="btn danger" id="btn-clear-history">Очистить историю</button>
      </div>

      <h2 style="margin-top:28px">Chrome Extension</h2>
      <div class="statusbox" id="ext-statusbox">
        <i class="dot" id="ext-dot"></i>
        <div>
          <strong id="ext-status-label">Проверка…</strong>
          <span id="ext-status-subtext">Передаются только домен и заголовок вкладки. Полный URL не сохраняется.</span>
        </div>
        <button class="btn" style="margin-left:auto" id="btn-check-ext">Обновить статус</button>
      </div>

    </div>
  `;
  hydrateIcons(body);

  // Toggle switches with click & Space key handling
  const swRecording = body.querySelector<HTMLButtonElement>("#sw-recording");
  const swAutostart = body.querySelector<HTMLButtonElement>("#sw-autostart");

  const bindSwitch = (btn: HTMLButtonElement | null, getValue: () => boolean, setValue: (val: boolean) => void) => {
    if (!btn) return;
    btn.onclick = () => {
      const nextVal = !getValue();
      setValue(nextVal);
      btn.classList.toggle("on", nextVal);
      btn.setAttribute("aria-checked", String(nextVal));
    };
    btn.onkeydown = (e) => {
      if (e.key === " " || e.key === "Spacebar") {
        e.preventDefault();
        btn.click();
      }
    };
  };

  bindSwitch(swRecording, () => !s.paused, (on) => {
    if (draftSettings) {
      draftSettings.paused = !on;
      settingsDirty = true;
    }
  });
  bindSwitch(swAutostart, () => s.autostart, (on) => {
    if (draftSettings) {
      draftSettings.autostart = on;
      settingsDirty = true;
    }
  });

  const inputShortcut = body.querySelector<HTMLInputElement>("#input-shortcut");
  inputShortcut?.addEventListener("input", () => {
    if (draftSettings) {
      draftSettings.shortcut = inputShortcut.value;
      settingsDirty = true;
    }
  });

  const inputRetention = body.querySelector<HTMLInputElement>("#input-retention");
  inputRetention?.addEventListener("input", () => {
    if (draftSettings) {
      draftSettings.retentionDays = Number(inputRetention.value);
      settingsDirty = true;
    }
  });

  // Save
  body.querySelector("#btn-save-settings")?.addEventListener("click", async () => {
    const next = draftSettings || s;
    try {
      await api.saveSettings(next);
      state.settings = { ...next };
      draftSettings = { ...next };
      settingsDirty = false;
      updateRecordingStatus(next.paused);
      const pauseBtn = document.querySelector<HTMLButtonElement>("#btn-pause");
      if (pauseBtn) pauseBtn.querySelector("span")!.textContent = next.paused ? "Продолжить" : "Пауза";
      showToast("Настройки сохранены", "info");
    } catch (err) {
      showToast(typeof err === "string" ? err : "Не удалось сохранить настройки");
    }
  });

  // Clear history
  body.querySelector("#btn-clear-history")?.addEventListener("click", async () => {
    const confirmed = await confirmModal(
      "Очистить историю?",
      "Удалить всю незакреплённую историю? Это действие нельзя отменить."
    );
    if (confirmed) {
      try {
        await api.clear();
        await reload();
        showToast("История очищена", "info");
      } catch {
        showToast("Не удалось очистить историю");
      }
    }
  });

  // Check extension
  async function refreshExtensionStatus() {
    try {
      const status = await api.getIntegrationStatus();
      const label = body.querySelector<HTMLElement>("#ext-status-label");
      const dot = body.querySelector<HTMLElement>("#ext-dot");
      const subtext = body.querySelector<HTMLElement>("#ext-status-subtext");
      if (status.nativeMessagingConnected) {
        if (label) label.textContent = "Подключено";
        if (dot) dot.className = "dot";
        if (subtext) subtext.textContent = "Связь с расширением активно подтверждена.";
      } else if (status.nativeMessagingConfigured) {
        if (label) label.textContent = "Расширение установлено, но связь не подтверждена";
        if (dot) dot.className = "dot paused";
        if (subtext) subtext.textContent = "Откройте или обновите обычную веб-страницу или нажмите кнопку в расширении.";
      } else {
        if (label) label.textContent = "Не подключено";
        if (dot) dot.className = "dot paused";
        if (subtext) subtext.textContent = "KitsuPin сейчас недоступен или манифест не настроен.";
      }
    } catch {
      // ignore
    }
  }

  body
    .querySelector("#btn-check-ext")
    ?.addEventListener("click", refreshExtensionStatus);

  void refreshExtensionStatus();

}

// ── Reload / bootstrap ────────────────────────────────────────────────────────

async function reload() {
  const data = await api.bootstrap(popup);
  state.categories = data.categories;
  state.settings = data.settings;
  if (!settingsDirty && data.settings) {
    draftSettings = { ...data.settings };
  }
  state.clips = await api.list(query());
  state.hasMore = !popup && state.clips.length === 60;
  renderFilters();
  renderCards();
  if (!popup) renderCategoriesScreen();

  // Update recording status indicator
  if (!popup && state.settings) updateRecordingStatus(state.settings.paused);

  // 6.4: show invalid-settings warning only once per session (not on each reload).
  if (!invalidWarningConsumed && !popup) {
    const shouldWarn = await api.consumeInvalidSettingsWarning();
    if (shouldWarn) {
      invalidWarningConsumed = true;
      setTimeout(() => {
        modal(
          "Предупреждение",
          `<div class="notice" style="border-left-color:var(--danger);background:color-mix(in oklch,var(--danger) 10%,transparent);color:oklch(30% 0.15 25);padding:12px;font-size:12px;line-height:1.5;">Файл настроек (settings.json) был повреждён или содержал недопустимые значения. Настройки были сброшены до безопасных значений (90 дней), а исходный файл сохранён как <b>settings.invalid.json</b>.</div>`
        );
      }, 100);
    }
  }
}

// ── Keyboard navigation ───────────────────────────────────────────────────────

document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && (e.key === "f" || e.key === "F" || e.key === "а" || e.key === "А")) {
    e.preventDefault();
    if (!popup) showScreen("history");
    const searchInput = document.querySelector<HTMLInputElement>("#search");
    if (searchInput) {
      searchInput.focus();
      searchInput.select();
    }
    return;
  }
  if (!popup) return;
  if (e.key === "Escape") {
    if (isTauri) api.hidePopup();
  } else if (e.key === "ArrowDown") {
    e.preventDefault();
    state.selected = Math.max(0, Math.min(state.selected + 1, state.clips.length - 1));
    renderCards();
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    state.selected = Math.max(0, state.selected - 1);
    renderCards();
  } else if (e.key === "Enter" && document.activeElement?.id === "search" && state.clips[state.selected]) {
    const clip = state.clips[state.selected]!;
    const doIt = async () => {
      try {
        if (isTauri) {
          await api.copy(clip.id, true);
        } else {
          const content = await api.getClipContent(clip.id);
          await api.copy(clip.id, true, content);
        }
      } catch {
        showToast("Не удалось скопировать");
      }
    };
    doIt();
  }
});

// ── Init ──────────────────────────────────────────────────────────────────────

renderShell();
reload();

if (isTauri) {
  listen("clips-changed", refresh);
  listen("categories-changed", reload);
  listen("settings-changed", reload);
  if (!popup) {
    listen("open-settings", showSettings);
    listen("confirm-clear-history", async () => {
      const confirmed = await confirmModal(
        "Очистить историю?",
        "Удалить всю незакреплённую историю? Это действие нельзя отменить."
      );
      if (confirmed) {
        try {
          await api.clear();
          await reload();
        } catch {
          showToast("Не удалось очистить историю");
        }
      }
    });
  } else {
    listen("popup-opened", () => {
      reload();
    });
    listen("popup-focused", () => {
      const searchInput = document.querySelector<HTMLInputElement>("#search");
      if (searchInput) {
        searchInput.focus();
      }
    });
  }
}
