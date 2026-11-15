import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  ArrowClockwise,
  CheckCircle,
  CircleNotch,
  DownloadSimple,
  FilmStrip,
  Check,
  FolderOpen,
  FolderSimple,
  Info,
  MagnifyingGlass,
  PencilSimple,
  Plus,
  SignOut,
  Trash,
  WarningCircle,
  X
} from "@phosphor-icons/react";
import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { AboutDialog } from "./components/AboutDialog";
import { MenuBar, type Menu } from "./components/MenuBar";
import { TitleBar } from "./components/TitleBar";
import { revealMainWindow } from "./lib/splash";
import { checkForUpdate, getAppVersion, type UpdateInfo } from "./lib/updater";

/** One video found on a page (see src-tauri/src/scrape.rs). */
interface VideoItem {
  title: string;
  url: string;
  thumbnail: string | null;
  source: string;
  direct: boolean;
  durationSeconds: number | null;
}

/** A found video, with the entry of the page list that it came from. */
interface ListedVideo extends VideoItem {
  page: string;
}

/** What the last read of a page gave. */
type PageState =
  | { kind: "reading" }
  | { kind: "done"; count: number }
  | { kind: "error"; message: string };

type DownloadState =
  | { kind: "running"; percent: number | null }
  | { kind: "done"; path: string }
  | { kind: "error"; message: string };

const PAGES_KEY = "scrapesage.pages";
const FOLDER_KEY = "scrapesage.downloadFolder";
/** The error text of a download that the user cancelled (see src-tauri/src/download.rs). */
const CANCELLED = "Cancelled";

/**
 * The mark in a page's address where the search text goes. A page with this
 * mark is a search page: it is the site's own search (often a JSON API), and
 * it is read each time the user starts a search.
 */
const SEARCH_MARK = "{query}";
/** Text that the address parser leaves as it is; it stands in for the mark while an address is checked. */
const SEARCH_MARK_STAND_IN = "SCRAPESAGESEARCHMARK";

/**
 * The mark in a page's address where the page number goes. A page with this
 * mark has more than one page of videos: the first read gets page 1, and
 * "Load more" gets the next page each time.
 */
const NUMBER_MARK = "{page}";
const NUMBER_MARK_STAND_IN = "SCRAPESAGENUMBERMARK";

const isSearchPage = (page: string) => page.includes(SEARCH_MARK);
const isNumberedPage = (page: string) => page.includes(NUMBER_MARK);

/** A page's address with the search text and the page number in it; spaces in the search text become "+". */
const readAddress = (page: string, text: string, number: number) =>
  page
    .replaceAll(SEARCH_MARK, encodeURIComponent(text.trim()).replace(/%20/g, "+"))
    .replaceAll(NUMBER_MARK, String(number));

function loadPages(): string[] {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(PAGES_KEY) ?? "[]");
    return Array.isArray(saved) ? saved.filter((page): page is string => typeof page === "string") : [];
  } catch {
    return [];
  }
}

/** A typed address as a full web address ("example.com/videos" gets "https://"), or null when it is not one. */
function webAddress(text: string): string | null {
  /* "YOUR+SEARCH+TERM", as API guides write it, is taken as the search mark. */
  const trimmed = text.trim().replace(/YOUR[+_ %20]*SEARCH[+_ %20]*TERM/gi, SEARCH_MARK).replaceAll(SEARCH_MARK, SEARCH_MARK_STAND_IN).replaceAll(NUMBER_MARK, NUMBER_MARK_STAND_IN);
  if (!trimmed) return null;
  try {
    const url = new URL(/^https?:\/\//i.test(trimmed) ? trimmed : `https://${trimmed}`);
    return url.hostname.includes(".") || url.hostname === "localhost" ? url.toString().replaceAll(SEARCH_MARK_STAND_IN, SEARCH_MARK).replaceAll(NUMBER_MARK_STAND_IN, NUMBER_MARK) : null;
  } catch {
    return null;
  }
}

function hostOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

function duration(seconds: number): string {
  const whole = Math.round(seconds);
  const hours = Math.floor(whole / 3600);
  const minutes = Math.floor((whole % 3600) / 60);
  const rest = String(whole % 60).padStart(2, "0");
  return hours > 0 ? `${hours}:${String(minutes).padStart(2, "0")}:${rest}` : `${minutes}:${rest}`;
}

/** A video's picture: its thumbnail, else the first frame of a video file, else an icon. */
function Thumbnail({ item }: { item: VideoItem }) {
  const [failed, setFailed] = useState(false);
  return (
    <div className="video-thumb">
      {item.thumbnail && !failed ? (
        /* No referrer: many sites refuse a picture request that names another site. */
        <img src={item.thumbnail} alt="" loading="lazy" referrerPolicy="no-referrer" onError={() => setFailed(true)} />
      ) : item.direct && !failed && !/\.(m3u8|mpd)(\?|$)/i.test(item.url) ? (
        <video src={`${item.url}#t=1`} preload="metadata" muted onError={() => setFailed(true)} />
      ) : (
        <FilmStrip size={30} />
      )}
      {item.durationSeconds !== null && <span className="video-duration">{duration(item.durationSeconds)}</span>}
    </div>
  );
}

export default function App() {
  const [pages, setPages] = useState<string[]>(loadPages);
  const [pageStates, setPageStates] = useState<Record<string, PageState>>({});
  const [items, setItems] = useState<ListedVideo[]>([]);
  const [newPage, setNewPage] = useState("");
  const [pageError, setPageError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  /** The search text that the search pages were last read with. */
  const [searched, setSearched] = useState("");
  const [downloads, setDownloads] = useState<Record<string, DownloadState>>({});
  const [aboutOpen, setAboutOpen] = useState(false);
  const [version, setVersion] = useState("");
  const [startupUpdate, setStartupUpdate] = useState<UpdateInfo | null>(null);
  /** The download folder that the user set, or null for the default (Videos\Scrape Sage). */
  const [folder, setFolder] = useState<string | null>(() => localStorage.getItem(FOLDER_KEY));
  const [defaultFolder, setDefaultFolder] = useState("");
  /** The page whose address is in edit, with the edited text and its error. */
  const [edit, setEdit] = useState<{ page: string; text: string; error: string | null } | null>(null);
  const newPageRef = useRef<HTMLInputElement>(null);
  /** Counts the reads of each page, so that an old read's answer does not replace a newer one's. */
  const readCounts = useRef<Record<string, number>>({});
  /** The last page number read of each numbered page. */
  const pageNumbers = useRef<Record<string, number>>({});
  /** The numbered pages whose last read gave no new video: they have no more pages. */
  const [ended, setEnded] = useState<Record<string, boolean>>({});
  /** The videos as of the last render, for a read that must count the new ones. */
  const itemsRef = useRef<ListedVideo[]>([]);
  itemsRef.current = items;

  useEffect(() => { void revealMainWindow(); }, []);

  useEffect(() => {
    localStorage.setItem(PAGES_KEY, JSON.stringify(pages));
  }, [pages]);

  useEffect(() => {
    if (folder === null) localStorage.removeItem(FOLDER_KEY);
    else localStorage.setItem(FOLDER_KEY, folder);
  }, [folder]);

  useEffect(() => {
    if (!isTauri()) return;
    invoke<string>("download_folder").then(setDefaultFolder).catch(() => {});
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    getAppVersion().then((value) => { if (!cancelled) setVersion(value); }).catch(() => {});
    checkForUpdate().then((info) => { if (!cancelled && info) setStartupUpdate(info); }).catch(() => {});
    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    const unlisten = listen<{ id: string; percent: number | null }>("download-progress", (event) => {
      const { id, percent } = event.payload;
      setDownloads((current) => current[id]?.kind === "running" ? { ...current, [id]: { kind: "running", percent } } : current);
    });
    return () => { void unlisten.then((stop) => stop()); };
  }, []);

  /**
   * Reads one page and puts its videos in the place of that page's earlier
   * videos. A search page is read with the search text in its address; with
   * no search text it has no videos.
   */
  const readPage = useCallback(async (page: string, text: string, more = false) => {
    const count = (readCounts.current[page] ?? 0) + 1;
    readCounts.current[page] = count;
    const current = () => readCounts.current[page] === count;
    if (isSearchPage(page) && text.trim() === "") {
      setItems((all) => all.filter((item) => item.page !== page));
      setPageStates(({ [page]: _waiting, ...rest }) => rest);
      return;
    }
    /* "Load more" reads the page after the last one read; each other read starts again at page 1. */
    const number = more ? (pageNumbers.current[page] ?? 1) + 1 : 1;
    setPageStates((states) => ({ ...states, [page]: { kind: "reading" } }));
    try {
      const found = await invoke<VideoItem[]>("scrape_videos", { url: readAddress(page, text, number) });
      if (!current()) return;
      pageNumbers.current[page] = number;
      const kept = more ? itemsRef.current.filter((item) => item.page === page) : [];
      const have = new Set(kept.map((item) => item.url));
      const fresh = found.filter((item) => !have.has(item.url)).map((item) => ({ ...item, page }));
      setItems((all) => [...all.filter((item) => more || item.page !== page), ...fresh]);
      setPageStates((states) => ({ ...states, [page]: { kind: "done", count: kept.length + fresh.length } }));
      setEnded((list) => ({ ...list, [page]: fresh.length === 0 }));
    } catch (error) {
      if (!current()) return;
      /* A failed "Load more" keeps the videos of the earlier pages. */
      if (!more) setItems((all) => all.filter((item) => item.page !== page));
      setPageStates((states) => ({ ...states, [page]: { kind: "error", message: String(error) } }));
    }
  }, []);

  const readAll = useCallback(() => {
    setSearched(query);
    for (const page of pages) void readPage(page, query);
  }, [pages, query, readPage]);

  /**
   * Enter in the search box, or the Search button: each search page is read
   * with the new search text, and each other page that was not read yet (or
   * whose read failed) is read. The other pages' videos only filter.
   */
  const startSearch = () => {
    setSearched(query);
    for (const page of pages) {
      const kind = pageStates[page]?.kind;
      if (isSearchPage(page) || kind === undefined || kind === "error") void readPage(page, query);
    }
  };

  const clearSearch = () => {
    setQuery("");
    setSearched("");
    for (const page of pages.filter(isSearchPage)) void readPage(page, "");
  };

  const addPage = (event: FormEvent) => {
    event.preventDefault();
    const page = webAddress(newPage);
    if (!page) {
      setPageError("Type a web address, such as https://example.com/videos.");
      return;
    }
    if (pages.includes(page)) {
      setPageError("This page is already in the list.");
      return;
    }
    setPageError(null);
    setNewPage("");
    setPages((list) => [...list, page]);
    void readPage(page, searched);
  };

  const removePage = (page: string) => {
    /* An answer still on its way for this page is dropped. */
    readCounts.current[page] = (readCounts.current[page] ?? 0) + 1;
    setPages((list) => list.filter((entry) => entry !== page));
    setItems((all) => all.filter((item) => item.page !== page));
    setPageStates(({ [page]: _removed, ...rest }) => rest);
  };

  /** Puts the edited address in the place of the page's old address, and reads it. */
  const saveEdit = () => {
    if (!edit) return;
    const page = webAddress(edit.text);
    if (!page) {
      setEdit({ ...edit, error: "Type a web address, such as https://example.com/videos." });
      return;
    }
    if (page === edit.page) {
      setEdit(null);
      return;
    }
    if (pages.includes(page)) {
      setEdit({ ...edit, error: "This page is already in the list." });
      return;
    }
    const old = edit.page;
    /* An answer still on its way for the old address is dropped. */
    readCounts.current[old] = (readCounts.current[old] ?? 0) + 1;
    setPages((list) => list.map((entry) => entry === old ? page : entry));
    setItems((all) => all.filter((item) => item.page !== old));
    setPageStates(({ [old]: _replaced, ...rest }) => rest);
    setEdit(null);
    void readPage(page, searched);
  };

  const removeAllPages = () => {
    for (const page of pages) readCounts.current[page] = (readCounts.current[page] ?? 0) + 1;
    setPages([]);
    setItems([]);
    setPageStates({});
  };

  const download = async (item: ListedVideo) => {
    setDownloads((current) => ({ ...current, [item.url]: { kind: "running", percent: null } }));
    try {
      const path = await invoke<string>("download_video", { id: item.url, url: item.url, title: item.title, source: item.source, direct: item.direct, folder });
      setDownloads((current) => ({ ...current, [item.url]: { kind: "done", path } }));
    } catch (error) {
      /* A cancelled download goes back to its Download button, with no error. */
      if (String(error) === CANCELLED) setDownloads(({ [item.url]: _cancelled, ...rest }) => rest);
      else setDownloads((current) => ({ ...current, [item.url]: { kind: "error", message: String(error) } }));
    }
  };

  const cancelDownload = (item: ListedVideo) => {
    void invoke("cancel_download", { id: item.url }).catch(() => {});
  };

  const openDownloadFolder = async () => {
    try {
      await invoke("open_folder", { path: folder ?? await invoke<string>("download_folder") });
    } catch (error) {
      console.error(error);
    }
  };

  /** Asks for the folder that downloads go to. Downloads that run now keep their folder. */
  const chooseFolder = async () => {
    try {
      const chosen = await openDialog({ directory: true, title: "Download folder", defaultPath: folder ?? (defaultFolder || undefined) });
      if (typeof chosen === "string") setFolder(chosen);
    } catch (error) {
      console.error(error);
    }
  };

  /**
   * A video shows when its title has every word of the search text; an empty
   * search shows all. A search page's videos always show: the site searched
   * already, and its matches can be on words that are not in the title.
   */
  const shown = useMemo(() => {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (words.length === 0) return items;
    return items.filter((item) => {
      if (isSearchPage(item.page)) return true;
      const title = item.title.toLowerCase();
      return words.every((word) => title.includes(word));
    });
  }, [items, query]);

  const reading = pages.some((page) => pageStates[page]?.kind === "reading");

  /** The numbered pages that can give one more page: read with no error, and not at their end. */
  const morePages = pages.filter((page) => isNumberedPage(page) && pageStates[page]?.kind === "done" && !ended[page]);
  const loadMore = () => {
    for (const page of morePages) void readPage(page, searched, true);
  };

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (aboutOpen) return;
      if (event.key === "F5") {
        event.preventDefault();
        readAll();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [aboutOpen, readAll]);

  const menus: Menu[] = [
    {
      id: "file",
      label: "File",
      items: [
        { label: "Add Page…", icon: <Plus size={15} />, run: () => newPageRef.current?.focus() },
        { label: "Read All Pages Again", icon: <ArrowClockwise size={15} />, shortcut: "F5", disabled: pages.length === 0, run: readAll },
        "separator",
        { label: "Set Download Folder…", icon: <FolderSimple size={15} />, run: () => void chooseFolder() },
        { label: "Use Default Download Folder", icon: <FolderSimple size={15} />, disabled: folder === null, run: () => setFolder(null) },
        { label: "Open Download Folder", icon: <FolderOpen size={15} />, run: () => void openDownloadFolder() },
        "separator",
        { label: "Exit", icon: <SignOut size={15} />, shortcut: "Alt+F4", run: () => void (isTauri() ? getCurrentWindow().close() : window.close()) }
      ]
    },
    {
      id: "edit",
      label: "Edit",
      items: [
        { label: "Clear Search", icon: <X size={15} />, disabled: query === "" && searched === "", run: clearSearch },
        { label: "Remove All Pages", icon: <Trash size={15} />, disabled: pages.length === 0, run: removeAllPages }
      ]
    },
    {
      id: "help",
      label: "Help",
      items: [
        { label: "About Scrape Sage", icon: <Info size={15} />, run: () => setAboutOpen(true) }
      ]
    }
  ];

  return (
    <div className="app">
      <TitleBar updateAvailable={Boolean(startupUpdate)} onAbout={() => setAboutOpen(true)} />
      <MenuBar menus={menus} />
      <div className="workspace">
        <aside className="pages-panel">
          <h2 className="panel-heading">Pages</h2>
          <form className="page-add" onSubmit={addPage}>
            <input
              ref={newPageRef}
              type="text"
              value={newPage}
              placeholder="https://example.com/search?q={query}&page={page}"
              spellCheck={false}
              onChange={(event) => { setNewPage(event.target.value); setPageError(null); }}
            />
            <button className="button primary small" type="submit" title="Add this page to the list"><Plus size={14} weight="bold" /> Add</button>
          </form>
          {pageError && <p className="page-add-error">{pageError}</p>}
          {pages.length === 0 ? (
            <div className="empty-state">
              <p>Add the address of a web page that has videos. Scrape Sage reads each page in this list and shows the videos it finds.</p>
              <p>To use a site's own search, put <code className="selectable">{SEARCH_MARK}</code> in the address where the search text goes. The app reads that address each time you search.</p>
              <p>For a site that gives its videos one page at a time, put <code className="selectable">{NUMBER_MARK}</code> where the page number goes. "Load more" then reads the next page.</p>
            </div>
          ) : (
            <ul className="page-list">
              {pages.map((page) => {
                const state = pageStates[page];
                if (edit?.page === page) {
                  return (
                    <li key={page} className="page-row editing">
                      <form className="page-edit" onSubmit={(event) => { event.preventDefault(); saveEdit(); }}>
                        <input
                          type="text"
                          value={edit.text}
                          autoFocus
                          spellCheck={false}
                          onChange={(event) => setEdit({ page, text: event.target.value, error: null })}
                          onKeyDown={(event) => { if (event.key === "Escape") setEdit(null); }}
                        />
                        <button type="submit" className="icon-button" title="Save the address (Enter)" aria-label="Save the address"><Check size={14} weight="bold" /></button>
                        <button type="button" className="icon-button" onClick={() => setEdit(null)} title="Keep the old address (Esc)" aria-label="Keep the old address"><X size={14} /></button>
                      </form>
                      {edit.error && <p className="page-add-error">{edit.error}</p>}
                    </li>
                  );
                }
                return (
                  <li key={page} className="page-row">
                    <div className="page-row-text">
                      <strong>{hostOf(page)}{isSearchPage(page) && <small className="page-kind">Search</small>}</strong>
                      <span className="selectable" title={page}>{page}</span>
                      {state?.kind === "reading" && <em><CircleNotch className="spin" size={12} /> Reading…</em>}
                      {state?.kind === "done" && <em>{state.count === 1 ? "1 video" : `${state.count} videos`}</em>}
                      {state?.kind === "error" && <em className="error selectable" title={state.message}><WarningCircle size={12} weight="fill" /> {state.message}</em>}
                      {!state && <em>{isSearchPage(page) ? "Type a search, then press Enter" : "Not read yet"}</em>}
                    </div>
                    <button className="icon-button" onClick={() => void readPage(page, query)} disabled={state?.kind === "reading" || (isSearchPage(page) && query.trim() === "")} title="Read this page again" aria-label="Read this page again"><ArrowClockwise size={14} /></button>
                    <button className="icon-button" onClick={() => setEdit({ page, text: page, error: null })} title="Change this page's address" aria-label="Change this page's address"><PencilSimple size={14} /></button>
                    <button className="icon-button danger" onClick={() => removePage(page)} title="Remove this page" aria-label="Remove this page"><X size={14} /></button>
                  </li>
                );
              })}
            </ul>
          )}
        </aside>
        <main className="results-panel">
          <div className="search-row">
            <form className="search-box" onSubmit={(event: FormEvent) => { event.preventDefault(); startSearch(); }}>
              <MagnifyingGlass size={16} />
              <input
                type="text"
                value={query}
                placeholder={pages.some(isSearchPage) ? "Type a search, then press Enter" : "Search the video titles"}
                spellCheck={false}
                onChange={(event) => setQuery(event.target.value)}
                /* Enter starts the search here, not only through the form, so it works in every state of the field. */
                onKeyDown={(event) => {
                  if (event.key !== "Enter") return;
                  event.preventDefault();
                  startSearch();
                }}
              />
              {query !== "" && <button type="button" className="icon-button" onClick={clearSearch} title="Clear the search" aria-label="Clear the search"><X size={14} /></button>}
            </form>
            <button className="button primary" onClick={startSearch} disabled={pages.length === 0} title="Search (Enter)">
              <MagnifyingGlass size={15} weight="bold" /> Search
            </button>
            <button className="button secondary" onClick={readAll} disabled={pages.length === 0 || reading} title="Read all pages again (F5)">
              {reading ? <CircleNotch className="spin" size={15} /> : <ArrowClockwise size={15} />} Read pages
            </button>
            <span className="result-count">{shown.length === items.length ? `${items.length} videos` : `${shown.length} of ${items.length} videos`}</span>
          </div>
          {shown.length === 0 ? (
            <div className="empty-state">
              <p>
                {pages.length === 0
                  ? "No pages yet. Add a page at the left to find its videos."
                  : reading
                    ? "Reading the pages…"
                    : items.length === 0 && pages.every(isSearchPage) && searched.trim() === ""
                      ? "Type a search and press Enter. The search pages in the list then look for videos."
                    : items.length === 0
                      ? "No videos found on the pages in the list. Add another page, or read the pages again."
                      : "No video title has all of these words. Change the search text to see more videos."}
              </p>
            </div>
          ) : (
            <ul className="video-list">
              {shown.map((item) => {
                const state = downloads[item.url];
                return (
                  <li key={`${item.page}\n${item.url}`} className="video-row">
                    <Thumbnail item={item} />
                    <div className="video-text">
                      <strong className="selectable" title={item.title}>{item.title}</strong>
                      <span className="selectable" title={item.url}>{hostOf(item.url)}</span>
                      {state?.kind === "running" && (
                        <div className="video-progress"><i className={state.percent === null ? "unknown" : ""} style={{ width: `${state.percent ?? 30}%` }} /></div>
                      )}
                      {state?.kind === "error" && <em className="error selectable"><WarningCircle size={12} weight="fill" /> {state.message}</em>}
                    </div>
                    {state?.kind === "running" ? (
                      <button className="button secondary cancel-button" onClick={() => cancelDownload(item)} title="Stop this download">
                        <CircleNotch className="spin" size={15} />
                        <span className="cancel-button-progress">{state.percent === null ? "Starting…" : `${Math.round(state.percent)}%`}</span>
                        <span className="cancel-button-label"><X size={13} weight="bold" /> Cancel</span>
                      </button>
                    ) : state?.kind === "done" ? (
                      <button className="button secondary" onClick={() => void invoke("reveal_file", { path: state.path }).catch(() => void openDownloadFolder())} title={state.path}>
                        <CheckCircle size={15} weight="fill" /> Show in folder
                      </button>
                    ) : (
                      <button className="button primary" onClick={() => void download(item)}>
                        <DownloadSimple size={15} weight="bold" /> {state?.kind === "error" ? "Try again" : "Download"}
                      </button>
                    )}
                  </li>
                );
              })}
              {(morePages.length > 0 || (reading && pages.some(isNumberedPage))) && (
                <li className="load-more-row">
                  <button className="button secondary" onClick={loadMore} disabled={reading} title="Read the next page of each page that has a page number">
                    {reading ? <CircleNotch className="spin" size={15} /> : <Plus size={15} weight="bold" />} {reading ? "Reading…" : "Load more"}
                  </button>
                </li>
              )}
            </ul>
          )}
          <footer className="folder-row">
            <FolderSimple size={15} />
            <span>Downloads go to</span>
            <strong className="selectable" title={folder ?? defaultFolder}>{folder ?? defaultFolder}</strong>
            <button className="button secondary small" onClick={() => void chooseFolder()}>Change…</button>
            <button className="button secondary small" onClick={() => void openDownloadFolder()}>Open</button>
          </footer>
        </main>
      </div>
      {aboutOpen && (
        <AboutDialog version={version} initialUpdateInfo={startupUpdate} onClose={() => setAboutOpen(false)} />
      )}
    </div>
  );
}
