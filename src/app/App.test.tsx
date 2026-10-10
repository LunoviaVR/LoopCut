import "@/ui/test/env";
import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { App } from "./App";

describe("App shell", () => {
  it("renders the editor regions and the wordmark", () => {
    render(<App />);

    expect(within(screen.getByRole("banner")).getByText("LoopCut")).toBeInTheDocument();
    expect(screen.getByRole("tablist", { name: "Tools" })).toBeInTheDocument();
    expect(screen.getByRole("main", { name: "Monitor" })).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "Inspector" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Timeline" })).toBeInTheDocument();
  });
});
