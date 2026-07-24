import anthropicSvg from "@lobehub/icons-static-svg/icons/claude.svg?raw";
import customSvg from "@lobehub/icons-static-svg/icons/newapi.svg?raw";
import deepseekSvg from "@lobehub/icons-static-svg/icons/deepseek.svg?raw";
import geminiSvg from "@lobehub/icons-static-svg/icons/gemini.svg?raw";
import glmSvg from "@lobehub/icons-static-svg/icons/zhipu.svg?raw";
import grokSvg from "@lobehub/icons-static-svg/icons/grok.svg?raw";
import kimiSvg from "@lobehub/icons-static-svg/icons/kimi.svg?raw";
import openaiSvg from "@lobehub/icons-static-svg/icons/openai.svg?raw";
import qwenSvg from "@lobehub/icons-static-svg/icons/qwen.svg?raw";

type AiBrand = {
  svg: string;
  color: string;
  title: string;
};

const aiBrands: Record<string, AiBrand> = {
  openai: { svg: openaiSvg, color: "#10A37F", title: "OpenAI" },
  anthropic: { svg: anthropicSvg, color: "#D97757", title: "Anthropic Claude" },
  kimi: { svg: kimiSvg, color: "#2864DC", title: "Kimi Moonshot" },
  deepseek: { svg: deepseekSvg, color: "#4D6BFE", title: "DeepSeek" },
  glm: { svg: glmSvg, color: "#385CE0", title: "Zhipu GLM" },
  grok: { svg: grokSvg, color: "#B7C2CC", title: "xAI Grok" },
  qwen: { svg: qwenSvg, color: "#6950EF", title: "Alibaba Qwen" },
  gemini: { svg: geminiSvg, color: "#8E75B2", title: "Google Gemini" },
  custom: { svg: customSvg, color: "#247EA6", title: "OpenAI-compatible service" },
};

export function aiProviderIcon(providerId: string, size = 24): string {
  const brand = aiBrands[providerId] ?? aiBrands.custom!;
  const svg = brand.svg
    .replace(/\s(?:height|width|style)="[^"]*"/g, "")
    .replace(/<title>.*?<\/title>/, `<title>${brand.title}</title>`);
  return svg.replace(
    "<svg ",
    `<svg class="ai-brand-icon" width="${size}" height="${size}" role="img" focusable="false" aria-label="${brand.title}" style="--ai-brand-color:${brand.color}" `,
  );
}

export function hasAiProviderIcon(providerId: string): boolean {
  return providerId in aiBrands;
}
