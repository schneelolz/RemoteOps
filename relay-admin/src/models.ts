/** 管理 API 的工具目录；此契约独立于 Agent 线协议。 */
export interface Platform {
  os: string;
  arch: string;
  os_versions?: string;
  status?: string;
  min_windows_build?: number;
  max_windows_build?: number;
}
export interface PlatformTest {
  os: string;
  arch: string;
  environment: string;
  desktop: boolean;
  status: string;
  result: string;
  tested_at: string;
}
export interface ToolPackage {
  tool: {
    id: string;
    version: string;
    platforms: Platform[];
    requires_desktop?: boolean;
    package: { size: number; sha256: string; url: string };
    startup: {
      interface: string;
      executable: string;
      args: string[];
      browser_adapter?: string;
      browser_executable?: string;
    };
    [key: string]: unknown;
  };
  source: { kind: string };
  file_verified: boolean;
  tests: PlatformTest[];
}
export interface ToolVersion {
  version: string;
  packages: ToolPackage[];
}
export interface ManagedTool {
  id: string;
  name: string;
  purpose: string;
  instructions: string;
  examples: string[];
  dependencies: string[];
  enabled: boolean;
  versions: ToolVersion[];
}
export type SyncStatus =
  | "synced"
  | "pending"
  | "update_pending"
  | "apply_failed"
  | "offline"
  | "upgrade_required";
export interface SyncNode {
  agent_instance_id: string;
  hostname: string;
  supported: boolean;
  online: boolean;
  applied_revision: number | null;
  synced: boolean;
  sync_status: SyncStatus;
}
export interface Catalog {
  revision: number;
  enabled: boolean;
  guide_markdown?: string;
  tools: ManagedTool[];
  nodes: SyncNode[];
  audit?: {
    created_at: string;
    actor: string;
    action: string;
    result: string;
    revision: number;
  }[];
}
