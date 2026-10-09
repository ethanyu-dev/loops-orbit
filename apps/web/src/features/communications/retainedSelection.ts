import type { Source } from './types';

// 与服务端批量限制一致，避免大列表产生无界事务。
export const MAX_RETAINED_SELECTION = 1000;

/** 选择时捕获版本与名称；后续轮询不更新用户的操作依据。 */
export type RetainedSelection = Pick<Source, 'id' | 'version' | 'label'>;

/** 全选明确针对当前搜索结果，分页不参与范围计算。 */
export function selectRetained(sources: Source[]): RetainedSelection[] {
  return sources
    .filter((source) => source.subscribed === false)
    .slice(0, MAX_RETAINED_SELECTION)
    .map(({ id, version, label }) => ({ id, version, label }));
}

/** 反选按身份移除，新增选择保留原有项的旧版本。 */
export function toggleRetained(selected: RetainedSelection[], source: Source) {
  if (selected.some((item) => item.id === source.id)) {
    return selected.filter((item) => item.id !== source.id);
  }
  if (source.subscribed !== false || selected.length >= MAX_RETAINED_SELECTION) return selected;
  return [...selected, ...selectRetained([source])];
}

/** 服务端仅接受身份与版本，展示名称不作为写入依据。 */
export function retainedVersions(selected: RetainedSelection[]) {
  return selected.map(({ id, version }) => ({ id, version }));
}
