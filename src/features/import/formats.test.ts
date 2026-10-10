import "@/ui/test/env";
import { describe, expect, it } from "vitest";
import {
  ensureProjectExtension,
  extensionOf,
  fileNameOf,
  pathKind,
  suggestedProjectFileName,
} from "./formats";

describe("file names and kinds", () => {
  it("takes the last path segment for both separators", () => {
    expect(fileNameOf("C:\\Clips\\ranked-ace.mp4")).toBe("ranked-ace.mp4");
    expect(fileNameOf("/home/user/clip.webm")).toBe("clip.webm");
    expect(fileNameOf("clip.mov")).toBe("clip.mov");
  });

  it("reads extensions case-insensitively and ignores dotfiles", () => {
    expect(extensionOf("C:\\a\\CLIP.MP4")).toBe("mp4");
    expect(extensionOf("C:\\a\\.hidden")).toBe("");
    expect(extensionOf("C:\\a\\noext")).toBe("");
  });

  it("classifies videos, projects and everything else", () => {
    expect(pathKind("C:\\a\\clip.MKV")).toBe("video");
    expect(pathKind("C:\\a\\clip.m2ts")).toBe("video");
    expect(pathKind("C:\\a\\reaction.loopcut")).toBe("project");
    expect(pathKind("C:\\a\\notes.txt")).toBe("unsupported");
    expect(pathKind("C:\\a\\picture.png")).toBe("unsupported");
  });

  it("adds the project extension only when it is missing", () => {
    expect(ensureProjectExtension("C:\\a\\reaction")).toBe("C:\\a\\reaction.loopcut");
    expect(ensureProjectExtension("C:\\a\\reaction.loopcut")).toBe("C:\\a\\reaction.loopcut");
  });

  it("suggests a file name without characters Windows rejects", () => {
    expect(suggestedProjectFileName("Ranked: ace/clip?")).toBe("Ranked_ ace_clip_.loopcut");
    expect(suggestedProjectFileName("   ")).toBe("Untitled.loopcut");
  });
});
