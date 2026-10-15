import type { OperationPlan, OperationResult } from "./types";

export interface InstallationFollowup {
  notice: string;
  pendingPlan?: OperationPlan;
}

export async function previewInstallationEnvironment(
  plan: OperationPlan,
  result: OperationResult,
  previewSwitch: (
    toolId: string,
    installationPath: string,
  ) => Promise<OperationPlan>,
): Promise<InstallationFollowup> {
  if (
    plan.actionKind !== "install" ||
    (result.status !== "committed" && result.status !== "committed_with_warning")
  ) {
    return { notice: result.message };
  }

  if (!result.installationPath) {
    return {
      notice: `${result.message}；未返回安装路径，环境尚未配置。请在已安装版本中点击“切换默认”。`,
    };
  }

  try {
    const pendingPlan = await previewSwitch(plan.toolId, result.installationPath);
    return {
      notice: `${result.message}；请确认默认版本计划以配置 PATH 和相关环境变量，或选择“仅保留安装”。`,
      pendingPlan,
    };
  } catch (error) {
    return {
      notice: `${result.message}；环境尚未配置，生成默认版本计划失败：${String(error)}。请在已安装版本中重试“切换默认”。`,
    };
  }
}
