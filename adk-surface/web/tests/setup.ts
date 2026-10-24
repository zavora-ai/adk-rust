import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

afterEach(() => {
  cleanup();
  // The conversation each thread follows lasts for the tab; every test starts a new one.
  window.sessionStorage.clear();
});
