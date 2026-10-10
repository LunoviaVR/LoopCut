import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { App } from "./App";

describe("App shell", () => {
  it("renders the editor regions", () => {
    render(<App />);
    expect(screen.getByText("LoopCut")).toBeInTheDocument();
    expect(screen.getByLabelText("Preview")).toBeInTheDocument();
    expect(screen.getByLabelText("Timeline")).toBeInTheDocument();
  });
});
