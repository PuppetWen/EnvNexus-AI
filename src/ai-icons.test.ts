import { describe, expect, it } from "vitest";
import { aiProviderIcon, hasAiProviderIcon } from "./ai-icons";

const providerIds = [
  "openai",
  "anthropic",
  "kimi",
  "deepseek",
  "glm",
  "grok",
  "qwen",
  "gemini",
  "custom",
];

describe("AI provider icons", () => {
  it("covers every built-in AI provider with a named SVG brand glyph", () => {
    for (const providerId of providerIds) {
      expect(hasAiProviderIcon(providerId)).toBe(true);
      expect(aiProviderIcon(providerId)).toContain("<svg");
      expect(aiProviderIcon(providerId)).toContain('class="ai-brand-icon"');
      expect(aiProviderIcon(providerId)).toContain("aria-label=");
      expect(aiProviderIcon(providerId)).not.toMatch(/>\s*[A-Za-z]{1,3}\s*</);
    }
  });

  it("uses the OpenAI knot and DeepSeek glyph instead of letter badges", () => {
    expect(aiProviderIcon("openai")).toContain('aria-label="OpenAI"');
    expect(aiProviderIcon("deepseek")).toContain('aria-label="DeepSeek"');
  });
});
