import "./test/env";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { Field } from "./Field";
import { Section, SectionHeading } from "./Section";
import { SegmentedControl } from "./SegmentedControl";
import { Select } from "./Select";
import { Switch } from "./Switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "./Tabs";

function SwitchHarness({ onChange }: { onChange: (on: boolean) => void }) {
  const [on, setOn] = useState(false);
  return (
    <Switch
      checked={on}
      label="Snap to frames"
      description="Round edits to whole output frames."
      onCheckedChange={(next) => {
        setOn(next);
        onChange(next);
      }}
    />
  );
}

describe("Switch", () => {
  it("is named by its label and described by its description", async () => {
    const onChange = vi.fn();
    render(<SwitchHarness onChange={onChange} />);

    const control = screen.getByRole("switch", { name: "Snap to frames" });
    expect(control).toHaveAttribute("aria-checked", "false");
    expect(control).toHaveAccessibleDescription("Round edits to whole output frames.");

    await userEvent.click(screen.getByText("Snap to frames"));
    expect(onChange).toHaveBeenCalledWith(true);
    expect(control).toHaveAttribute("aria-checked", "true");
  });
});

describe("Switch hit area", () => {
  it("grows the hit box to 24px while the track stays 16px and keeps its edge token", () => {
    render(<SwitchHarness onChange={vi.fn()} />);

    const control = screen.getByRole("switch", { name: "Snap to frames" });
    expect(control).toHaveClass("h-4", "before:-inset-[5px]", "border-line-control");
    expect(control).toHaveClass("hover:border-fg-muted");
  });

  it("outlines the thumb under forced colors, where its fill is dropped", () => {
    render(<SwitchHarness onChange={vi.fn()} />);

    const thumb = screen.getByRole("switch", { name: "Snap to frames" }).firstElementChild;
    expect(thumb).toHaveClass("forced-colors:outline-2", "forced-colors:outline-solid");
  });
});

function SegmentHarness({ onChange }: { onChange: (value: string) => void }) {
  const [value, setValue] = useState<"forward" | "reverse" | "pingPong">("forward");
  return (
    <SegmentedControl
      aria-label="Direction"
      value={value}
      items={[
        { value: "forward", label: "Forward" },
        { value: "reverse", label: "Reverse" },
        { value: "pingPong", label: "Ping-pong" },
      ]}
      onValueChange={(next) => {
        setValue(next);
        onChange(next);
      }}
    />
  );
}

describe("SegmentedControl", () => {
  it("selects one item and reports it as checked", async () => {
    const onChange = vi.fn();
    render(<SegmentHarness onChange={onChange} />);

    expect(screen.getByRole("radiogroup", { name: "Direction" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Forward" })).toHaveAttribute("aria-checked", "true");

    await userEvent.click(screen.getByRole("radio", { name: "Reverse" }));
    expect(onChange).toHaveBeenCalledWith("reverse");
    expect(screen.getByRole("radio", { name: "Reverse" })).toHaveAttribute("aria-checked", "true");
  });

  it("keeps the choice when the selected item is clicked again", async () => {
    const onChange = vi.fn();
    render(<SegmentHarness onChange={onChange} />);

    await userEvent.click(screen.getByRole("radio", { name: "Forward" }));

    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole("radio", { name: "Forward" })).toHaveAttribute("aria-checked", "true");
  });
});

function TabsHarness({ orientation }: { orientation: "horizontal" | "vertical" }) {
  const [value, setValue] = useState("trim");
  return (
    <Tabs value={value} onValueChange={setValue} orientation={orientation}>
      <TabsList aria-label="Sections">
        <TabsTrigger value="media">Media</TabsTrigger>
        <TabsTrigger value="trim">Trim</TabsTrigger>
        <TabsTrigger value="crop">Crop</TabsTrigger>
      </TabsList>
      <TabsContent value="media">Media panel</TabsContent>
      <TabsContent value="trim">Trim panel</TabsContent>
      <TabsContent value="crop">Crop panel</TabsContent>
    </Tabs>
  );
}

describe("Tabs", () => {
  it("moves between horizontal tabs with the arrow keys and selects on focus", async () => {
    render(<TabsHarness orientation="horizontal" />);

    const trim = screen.getByRole("tab", { name: "Trim" });
    trim.focus();
    await userEvent.keyboard("{ArrowRight}");

    expect(screen.getByRole("tab", { name: "Crop" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("Crop panel")).toBeVisible();
  });

  it("uses the up and down arrows for vertical tabs", async () => {
    render(<TabsHarness orientation="vertical" />);

    screen.getByRole("tab", { name: "Trim" }).focus();
    await userEvent.keyboard("{ArrowUp}");

    expect(screen.getByRole("tab", { name: "Media" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("tablist", { name: "Sections" })).toHaveAttribute(
      "aria-orientation",
      "vertical",
    );
  });
});

describe("Tabs layout", () => {
  it("draws vertical tabs as 32px full-width rows, not 40px squares", () => {
    render(<TabsHarness orientation="vertical" />);

    const trim = screen.getByRole("tab", { name: "Trim" });
    expect(trim).toHaveClass(
      "data-[orientation=vertical]:h-8",
      "data-[orientation=vertical]:w-full",
    );
    expect(trim.className).not.toContain("data-[orientation=vertical]:h-10");
    expect(trim.className).not.toContain("data-[orientation=vertical]:w-10");
  });

  it("marks the active vertical tab with the shared selected look", () => {
    render(<TabsHarness orientation="vertical" />);

    expect(screen.getByRole("tab", { name: "Trim" })).toHaveClass(
      "data-[orientation=vertical]:data-[state=active]:bg-accent-soft",
      "data-[orientation=vertical]:data-[state=active]:shadow-[inset_0_0_0_1px_var(--color-accent)]",
    );
  });
});

describe("SectionHeading", () => {
  it("is a fixed heading row with no toggle", () => {
    render(<SectionHeading title="Exports" />);

    expect(screen.getByRole("heading", { level: 3, name: "Exports" })).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });
});

describe("Section", () => {
  it("toggles its content from the heading button and reports aria-expanded", async () => {
    render(
      <Section title="Source">
        <p>File and resolution</p>
      </Section>,
    );

    const heading = screen.getByRole("button", { name: "Source" });
    expect(heading).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("File and resolution")).toBeVisible();

    await userEvent.click(heading);
    expect(heading).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("File and resolution")).not.toBeInTheDocument();
  });
});

describe("Select", () => {
  it("names the trigger and shows the selected option", () => {
    render(
      <Select
        aria-label="Size preset"
        value="discord"
        onValueChange={vi.fn()}
        options={[
          { value: "none", label: "No size limit" },
          { value: "discord", label: "Discord, 10 MB" },
        ]}
      />,
    );

    const trigger = screen.getByRole("combobox", { name: "Size preset" });
    expect(trigger).toHaveTextContent("Discord, 10 MB");
  });

  it("draws its resting edge with the control token and the hover step", () => {
    render(
      <Select
        aria-label="Size preset"
        value="discord"
        onValueChange={vi.fn()}
        options={[{ value: "discord", label: "Discord, 10 MB" }]}
      />,
    );

    expect(screen.getByRole("combobox", { name: "Size preset" })).toHaveClass(
      "border-line-control",
      "hover:border-fg-muted",
    );
  });
});

describe("Field", () => {
  it("associates its label with the control it names", () => {
    render(
      <Field label="Codec" htmlFor="codec">
        <input id="codec" defaultValue="h264" />
      </Field>,
    );

    expect(screen.getByRole("textbox", { name: "Codec" })).toHaveValue("h264");
  });

  it("puts the label in the shared 7rem column with the 12px label style", () => {
    render(
      <Field label="Codec" htmlFor="codec">
        <input id="codec" defaultValue="h264" />
      </Field>,
    );

    const label = screen.getByText("Codec");
    expect(label).toHaveClass("text-xs", "text-fg-muted");
    expect(label.parentElement).toHaveClass("grid-cols-[7rem_minmax(0,1fr)]");
  });

  it("stacks the label above a full-width control when laid out stacked", () => {
    render(
      <Field label="Font" htmlFor="font" layout="stacked" hint="Search by name.">
        <input id="font" defaultValue="Segoe UI" />
      </Field>,
    );

    const label = screen.getByText("Font");
    expect(label.parentElement).toHaveClass("flex-col");
    expect(label.parentElement).not.toHaveClass("grid");
    expect(screen.getByRole("textbox", { name: "Font" })).toHaveValue("Segoe UI");
    expect(screen.getByText("Search by name.")).toBeInTheDocument();
  });
});
