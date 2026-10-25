/**
 * The website's client (spec 3.5 §8.2): the desk's routes under `/api/surface/website`, with
 * the home's token. Onboarding sends the paired device's token, which the setup code gave this
 * tab (A-I20); every other route reads and acts as the person the server authenticated.
 */
import type {
  CharterCommand,
  CharterResponse,
  ClaimedView,
  DraftView,
  EditorCommand,
  EditorHome,
  EditorOutcome,
  OnboardingCommand,
  OnboardingOutcome,
  OnboardingView,
  ReviewCommand,
  ReviewPage,
  SetupView,
  SiteCommand,
  SiteOutcome,
  SiteViewModel,
  UploadRequest,
  WebsiteHome,
} from "../generated/website";
import { ApiError } from "../client";
import { readStored, writeStored } from "../stored";

/** Where the home keeps the server's token; the setup code's token is kept there too. */
const TOKEN_KEY = "adk.home.token";

export interface WebsiteClient {
  /** The site and the caller's roles; `undefined` when the server serves no website. */
  home(): Promise<WebsiteHome | undefined>;
  setup(): Promise<SetupView>;
  /** Claims the setup code; this tab keeps the device's token from then on. */
  claim(code: string, device: string): Promise<ClaimedView>;
  onboarding(): Promise<OnboardingView>;
  onboard(command: OnboardingCommand): Promise<OnboardingOutcome>;
  signCharter(command: CharterCommand): Promise<CharterResponse>;
  site(): Promise<SiteViewModel>;
  decideSite(command: SiteCommand): Promise<SiteOutcome>;
  review(week?: string): Promise<ReviewPage>;
  decideReview(command: ReviewCommand): Promise<ReviewPage>;
  editor(): Promise<EditorHome>;
  draft(id: string): Promise<DraftView>;
  edit(command: EditorCommand): Promise<EditorOutcome>;
  upload(draft: string, file: UploadRequest): Promise<DraftView>;
}

async function problem(response: Response): Promise<ApiError> {
  const text = await response.text();
  try {
    const value = JSON.parse(text) as { error?: { message?: string; code?: string } };
    return new ApiError(response.status, value.error?.message ?? text, value.error?.code);
  } catch {
    return new ApiError(response.status, text || `${response.status} ${response.statusText}`);
  }
}

/** The website's routes over HTTP, relative to the page's own server. */
export class HttpWebsiteClient implements WebsiteClient {
  constructor(
    private readonly base = "/api/surface/website",
    private readonly onToken?: (token: string) => void,
  ) {}

  private async request<T>(path: string, init?: RequestInit): Promise<T> {
    const token = readStored<string | undefined>(TOKEN_KEY, undefined);
    const response = await fetch(`${this.base}${path}`, {
      credentials: "same-origin",
      ...init,
      headers: {
        accept: "application/json",
        ...(init?.body ? { "content-type": "application/json" } : {}),
        ...(token ? { authorization: `Bearer ${token}` } : {}),
      },
    });
    if (!response.ok) throw await problem(response);
    return (await response.json()) as T;
  }

  private post<T>(path: string, body: unknown): Promise<T> {
    return this.request<T>(path, { method: "POST", body: JSON.stringify(body) });
  }

  async home() {
    try {
      return await this.request<WebsiteHome>("");
    } catch (error) {
      if (error instanceof ApiError && (error.status === 404 || error.status === 405)) return undefined;
      throw error;
    }
  }

  setup() {
    return this.request<SetupView>("/setup");
  }

  async claim(code: string, device: string) {
    const claimed = await this.post<ClaimedView>("/setup/claim", { code, device });
    writeStored(TOKEN_KEY, claimed.token);
    this.onToken?.(claimed.token);
    return claimed;
  }

  onboarding() {
    return this.request<OnboardingView>("/onboarding");
  }

  onboard(command: OnboardingCommand) {
    return this.post<OnboardingOutcome>("/onboarding", command);
  }

  signCharter(command: CharterCommand) {
    return this.post<CharterResponse>("/charter", command);
  }

  site() {
    return this.request<SiteViewModel>("/site");
  }

  decideSite(command: SiteCommand) {
    return this.post<SiteOutcome>("/site", command);
  }

  review(week?: string) {
    return this.request<ReviewPage>(week ? `/review?week=${encodeURIComponent(week)}` : "/review");
  }

  decideReview(command: ReviewCommand) {
    return this.post<ReviewPage>("/review", command);
  }

  editor() {
    return this.request<EditorHome>("/editor");
  }

  draft(id: string) {
    return this.request<DraftView>(`/editor/drafts/${encodeURIComponent(id)}`);
  }

  edit(command: EditorCommand) {
    return this.post<EditorOutcome>("/editor", command);
  }

  upload(draft: string, file: UploadRequest) {
    return this.post<DraftView>(`/editor/drafts/${encodeURIComponent(draft)}/media`, file);
  }
}

/** A file as base64, for an upload. */
export async function fileBase64(file: Blob): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}
