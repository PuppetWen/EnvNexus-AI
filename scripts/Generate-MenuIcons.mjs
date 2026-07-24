import { mkdir, readFile } from "node:fs/promises";
import { resolve } from "node:path";
import sharp from "sharp";
import { icons as lucideIcons } from "lucide";
import {
  siAndroid,
  siApachemaven,
  siCmake,
  siDotnet,
  siGit,
  siGo,
  siGradle,
  siNodedotjs,
  siPhp,
  siPython,
  siRuby,
  siRust,
} from "simple-icons";

const outputDirectory = resolve("src-tauri", "icons", "menu");
const javaBrandSvg = await readFile(resolve("src", "java-brand.svg"), "utf8");
const aiIconDirectory = resolve(
  "node_modules",
  "@lobehub",
  "icons-static-svg",
  "icons",
);

const toolIcons = {
  python: [siPython, "#3776AB"],
  java: [javaBrandSvg, null],
  go: [siGo, "#00ADD8"],
  rust: [siRust, "#C4552D"],
  node: [siNodedotjs, "#5FA04E"],
  git: [siGit, "#F05032"],
  maven: [siApachemaven, "#C71A36"],
  dotnet: [siDotnet, "#512BD4"],
  ruby: [siRuby, "#CC342D"],
  php: [siPhp, "#777BB4"],
  "android-sdk": [siAndroid, "#3DDC84"],
  "android-ndk": [siAndroid, "#27AE60"],
  gradle: [siGradle, "#20A39E"],
  cmake: [siCmake, "#1684C7"],
  adb: [siAndroid, "#159957"],
};

const actionIcons = {
  open: ["PanelTopOpen", "#247EA6"],
  dashboard: ["LayoutDashboard", "#247EA6"],
  tools: ["Blocks", "#247EA6"],
  diagnostics: ["Activity", "#D97706"],
  settings: ["Settings2", "#725AC1"],
  scan: ["Radar", "#159A80"],
  exit: ["LogOut", "#D14343"],
  version: ["GitBranch", "#60758D"],
  default: ["CircleCheck", "#1F9D68"],
  view: ["ClipboardCheck", "#247EA6"],
  repair: ["Wrench", "#D97706"],
  warning: ["TriangleAlert", "#D97706"],
  error: ["CircleX", "#D14343"],
  info: ["Info", "#247EA6"],
  ai: ["BrainCircuit", "#159A80"],
};

const aiIcons = {
  "ai-openai": ["openai.svg", "#10A37F"],
  "ai-anthropic": ["claude.svg", "#D97757"],
  "ai-kimi": ["kimi.svg", "#2864DC"],
  "ai-deepseek": ["deepseek.svg", "#4D6BFE"],
  "ai-glm": ["zhipu.svg", "#385CE0"],
  "ai-grok": ["grok.svg", "#242424"],
  "ai-qwen": ["qwen.svg", "#6950EF"],
  "ai-gemini": ["gemini.svg", "#8E75B2"],
  "ai-custom": ["newapi.svg", "#247EA6"],
};

function escapeXml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll('"', "&quot;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}

function simpleIconSvg(icon, color) {
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
    <path transform="translate(4 4)" fill="${color}" d="${icon.path}"/>
  </svg>`;
}

function lucideSvg(iconName, color) {
  const nodes = lucideIcons[iconName];
  const children = nodes
    .map(([tag, attributes]) => {
      const serialized = Object.entries(attributes)
        .map(([key, value]) => `${key}="${escapeXml(value)}"`)
        .join(" ");
      return `<${tag} ${serialized}/>`;
    })
    .join("");
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="${color}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${children}</svg>`;
}

async function writePng(name, svg) {
  await sharp(Buffer.from(svg))
    .resize(32, 32, { fit: "contain" })
    .png()
    .toFile(resolve(outputDirectory, `${name}.png`));
}

await mkdir(outputDirectory, { recursive: true });

for (const [name, [icon, color]] of Object.entries(toolIcons)) {
  await writePng(name, typeof icon === "string" ? icon : simpleIconSvg(icon, color));
}

for (const [name, [iconName, color]] of Object.entries(actionIcons)) {
  await writePng(name, lucideSvg(iconName, color));
}

for (const [name, [filename, color]] of Object.entries(aiIcons)) {
  const svg = await readFile(resolve(aiIconDirectory, filename), "utf8");
  await writePng(name, svg.replaceAll("currentColor", color));
}

console.log(
  `Generated ${Object.keys(toolIcons).length + Object.keys(actionIcons).length + Object.keys(aiIcons).length} menu icons in ${outputDirectory}`,
);
