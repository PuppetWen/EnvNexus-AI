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
  type SimpleIcon,
} from "simple-icons";
import javaBrandSvg from "./java-brand.svg?raw";

type ToolBrand = {
  icon?: SimpleIcon;
  customSvg?: string;
  title?: string;
  color: string;
  accentIcon?: "terminal" | "cpu" | "package";
};

const brands: Record<string, ToolBrand> = {
  python: { icon: siPython, color: "#3776AB" },
  java: { customSvg: javaBrandSvg, title: "Java", color: "#E76F00" },
  go: { icon: siGo, color: "#00ADD8" },
  rust: { icon: siRust, color: "#C4552D" },
  node: { icon: siNodedotjs, color: "#5FA04E" },
  git: { icon: siGit, color: "#F05032" },
  maven: { icon: siApachemaven, color: "#C71A36" },
  dotnet: { icon: siDotnet, color: "#512BD4" },
  ruby: { icon: siRuby, color: "#CC342D" },
  php: { icon: siPhp, color: "#777BB4" },
  "android-sdk": { icon: siAndroid, color: "#3DDC84", accentIcon: "package" },
  "android-ndk": { icon: siAndroid, color: "#3DDC84", accentIcon: "cpu" },
  gradle: { icon: siGradle, color: "#20A39E" },
  cmake: { icon: siCmake, color: "#1684C7" },
  adb: { icon: siAndroid, color: "#3DDC84", accentIcon: "terminal" },
};

const accentPaths: Record<NonNullable<ToolBrand["accentIcon"]>, string> = {
  terminal: "M3 5.5 6 8.5 3 11.5M7.5 12h4",
  cpu: "M5 5h6v6H5zM7 2.5v2M9 2.5v2M7 11.5v2M9 11.5v2M2.5 7h2M11.5 7h2M2.5 9h2M11.5 9h2",
  package: "m3 5 5-2.5L13 5 8 7.5 3 5Zm0 0v6L8 13.5 13 11V5M8 7.5v6",
};

export function toolBrandIcon(toolId: string, size = 28): string {
  const brand = brands[toolId];
  if (!brand) return "";
  if (brand.customSvg) {
    return brand.customSvg.replace(
      "<svg ",
      `<svg class="brand-icon-svg brand-icon-svg-original" width="${size}" height="${size}" role="img" aria-label="${brand.title ?? toolId}" `,
    );
  }
  const accent = brand.accentIcon
    ? `<g class="brand-icon-accent" transform="translate(10 10) scale(.82)"><circle cx="8" cy="8" r="7.2"></circle><path d="${accentPaths[brand.accentIcon]}"></path></g>`
    : "";
  return `<svg class="brand-icon-svg" width="${size}" height="${size}" viewBox="0 0 24 24" role="img" aria-label="${brand.icon?.title ?? toolId}" style="--brand-color:${brand.color}">
    <path class="brand-icon-main" d="${brand.icon?.path ?? ""}"></path>
    ${accent}
  </svg>`;
}

export function hasToolBrandIcon(toolId: string): boolean {
  return toolId in brands;
}
