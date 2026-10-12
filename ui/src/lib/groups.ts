/** UI spike: explicit organizational metadata, independent of execution. */
import type { AssetSummary, NodeState } from './types'
import { shortName, type GraphEntry } from './graph'
import { graphState, UNKNOWN_GRAPH_STATE, type GraphState } from './graphState'

export interface NodeGroup {
  id: string
  name: string
  description: string
  members: string[]
  output: string
}

export interface GroupTree {
  groups: Map<string, NodeGroup>
  roots: string[]
  parents: Map<string, string>
}

export const MODELING_PIPELINE = 'modeling.py'
export const PREVIEW_FAILURE =
  'modeling.py:validate__cv__fold_03__trained_model'
const nodeId = (name: string) => `${MODELING_PIPELINE}:${name}`

/** Membership is explicit. Prefixes just keep this demo definition readable. */
export function modelingGroups(): GroupTree {
  const groups = new Map<string, NodeGroup>()
  const steps = (names: string[]) =>
    names.flatMap((name) => [nodeId(name), nodeId(`validate__${name}`)])
  const group = (
    key: string,
    name: string,
    members: string[],
    output: string,
    description: string,
  ) => {
    const id = `group:${key}`
    groups.set(id, { id, name, members, output, description })
    return id
  }
  const source = group(
    'source',
    'Source data',
    steps([
      'data__source_spec',
      'data__raw_rows',
      'data__schema_profile',
      'data__clean_rows',
      'data__feature_contract',
    ]),
    nodeId('data__clean_rows'),
    'Generate, profile and clean the synthetic source rows.',
  )
  const split = group(
    'split',
    'Train / test split',
    steps([
      'data__split_manifest',
      'data__train_rows',
      'data__test_rows',
      'data__training_labels',
      'data__test_labels',
      'data__fold_assignments',
    ]),
    nodeId('data__train_rows'),
    'Hold out the test set and assign training rows to five folds.',
  )
  const preparation = group(
    'preparation',
    'Training preparation',
    steps([
      'data__training_profile',
      'data__imputation_values',
      'data__training_configuration',
    ]),
    nodeId('data__imputation_values'),
    'Profile training data and prepare the final training configuration.',
  )
  const data = group(
    'training_data',
    'Training data',
    [source, split, preparation],
    split,
    'Shared preparation and checks. Its primary output is the training rows; the test split remains available to downstream evaluation.',
  )

  const folds = Array.from({ length: 5 }, (_, index) => {
    const n = String(index + 1).padStart(2, '0')
    const prefix = `cv__fold_${n}__`
    const preprocess = group(
      `fold_${n}/preprocessing`,
      'Preprocessing',
      steps(
        [
          'train_rows',
          'validation_rows',
          'scaler',
          'train_features',
          'validation_features',
        ].map((s) => prefix + s),
      ),
      nodeId(prefix + 'train_features'),
      'Split this fold, fit preprocessing on its training rows, then transform both partitions.',
    )
    const fitting = group(
      `fold_${n}/fitting`,
      'Model fitting',
      steps(
        ['initial_weights', 'fitted_weights', 'trained_model'].map(
          (s) => prefix + s,
        ),
      ),
      nodeId(prefix + 'trained_model'),
      'Initialize, fit and package the fold model. Every asset has a validation task.',
    )
    const evaluation = group(
      `fold_${n}/evaluation`,
      'Evaluation',
      steps(['predictions', 'metrics'].map((s) => prefix + s)),
      nodeId(prefix + 'metrics'),
      'Predict on this fold’s validation partition and compute metrics.',
    )
    return group(
      `fold_${n}`,
      `Fold ${index + 1}`,
      [preprocess, fitting, evaluation],
      evaluation,
      'One cross-validation fold, including all preprocessing, fitting, evaluation and validation tasks.',
    )
  })
  const summary = group(
    'cv_summary',
    'Fold summary',
    steps(['cv__summary__fold_metrics', 'cv__summary__report']),
    nodeId('cv__summary__report'),
    'Aggregate all five folds into the cross-validation report.',
  )
  const cv = group(
    'cross_validation',
    'Cross-validation',
    [...folds, summary],
    summary,
    'Five independently cached folds. Any failed check makes this entire group unhealthy.',
  )
  const finalPrep = group(
    'final_preparation',
    'Final preprocessing',
    steps([
      'train__validated_configuration',
      'train__final_scaler',
      'train__final_features',
    ]),
    nodeId('train__final_features'),
    'Use cross-validation results and fit preprocessing on the full training partition.',
  )
  const finalFit = group(
    'final_fitting',
    'Model fitting',
    steps([
      'train__initial_weights',
      'train__fitted_weights',
      'train__trained_model',
    ]),
    nodeId('train__trained_model'),
    'Fit and package the final model, alongside its validation checks.',
  )
  const training = group(
    'final_training',
    'Final training',
    [finalPrep, finalFit],
    finalFit,
    'Train the final model using the training partition only.',
  )
  const test = group(
    'test_evaluation',
    'Test evaluation',
    steps(['test__features', 'test__predictions', 'test__metrics']),
    nodeId('test__metrics'),
    'Transform and evaluate the held-out test set.',
  )
  const release = group(
    'release',
    'Model card',
    steps(['release__model_card']),
    nodeId('release__model_card'),
    'Summarize the trained model, training data, cross-validation and test performance.',
  )
  const parents = new Map<string, string>()
  for (const g of groups.values())
    for (const member of g.members) parents.set(member, g.id)
  return { groups, roots: [data, cv, training, test, release], parents }
}

export const MODELING_GROUPS = modelingGroups()

export function groupLeaves(id: string, tree: GroupTree): string[] {
  const group = tree.groups.get(id)
  return group
    ? group.members.flatMap((member) => groupLeaves(member, tree))
    : [id]
}

export function groupOutput(id: string, tree: GroupTree): string {
  const group = tree.groups.get(id)
  return group ? groupOutput(group.output, tree) : id
}

export function groupPath(id: string | null, tree: GroupTree): NodeGroup[] {
  if (!id) return []
  const parent = tree.parents.get(id)
  const group = tree.groups.get(id)
  return [...groupPath(parent ?? null, tree), ...(group ? [group] : [])]
}

export function memberName(id: string): string {
  const name = shortName(id)
  const check = name.startsWith('validate__')
  const parts = name.replace(/^validate__/, '').split('__')
  return `${check ? 'Check · ' : ''}${parts.at(-1)?.replaceAll('_', ' ') ?? name}`
}

export function memberHealth(node: NodeState | undefined): GraphState {
  if (!node) return UNKNOWN_GRAPH_STATE
  if (
    node.kind !== 'asset' &&
    node.last_materialization?.status === 'success'
  ) {
    return {
      status: 'success',
      label: 'Passed',
      hint: 'The latest execution succeeded.',
    }
  }
  return graphState(node)
}

export function groupHealth(
  ids: string[],
  states: Map<string, NodeState>,
): GraphState {
  const members = ids.map((id) => memberHealth(states.get(id)))
  const failed = members.filter((s) => s.status === 'failed').length
  const warning = members.filter((s) => s.status === 'warning').length
  const unknown = members.filter((s) => s.status === 'skipped').length
  if (failed)
    return {
      status: 'failed',
      label: `${failed} failed member${failed === 1 ? '' : 's'}`,
      hint: 'A failure anywhere inside this group makes it unhealthy, including failed checks on cached assets.',
    }
  if (warning)
    return {
      status: 'warning',
      label: `${warning} stale / partial`,
      hint: 'One or more member assets need attention.',
    }
  if (unknown)
    return {
      status: 'skipped',
      label: `${unknown} unchecked / unknown`,
      hint: 'Some members have not run or their state is unavailable.',
    }
  return {
    status: 'success',
    label: 'Healthy',
    hint: 'All assets are cached and all checks have passed.',
  }
}

/** Collapse only visual edges. Real inputs and the original graph stay intact. */
export function projectGroups(
  assets: AssetSummary[],
  tree: GroupTree,
  scope: string | null,
) {
  const byId = new Map(assets.map((a) => [a.id, a]))
  const members = scope
    ? (tree.groups.get(scope)?.members ?? [])
    : [
        ...tree.roots.filter((id) =>
          groupLeaves(id, tree).some((leaf) => byId.has(leaf)),
        ),
        ...assets.filter((a) => !tree.parents.has(a.id)).map((a) => a.id),
      ]
  const owner = new Map<string, string>()
  for (const id of members)
    for (const leaf of groupLeaves(id, tree)) owner.set(leaf, id)
  const external = new Set<string>()
  const entries: GraphEntry[] = members
    .filter((id) => tree.groups.has(id) || byId.has(id))
    .map((id) => {
      const group = tree.groups.get(id)
      const leaves = groupLeaves(id, tree).filter((leaf) => byId.has(leaf))
      const inputs = new Set<string>()
      for (const leaf of leaves)
        for (const input of byId.get(leaf)?.inputs ?? []) {
          const visibleOwner = owner.get(input)
          if (visibleOwner && visibleOwner !== id) inputs.add(visibleOwner)
          else if (!visibleOwner) external.add(input)
        }
      const assetCount = leaves.filter(
        (leaf) => byId.get(leaf)?.kind === 'asset',
      ).length
      return {
        id,
        kind: group ? 'group' : byId.get(id)!.kind,
        inputs: [...inputs],
        name: group?.name ?? (scope ? memberName(id) : shortName(id)),
        summary: `${assetCount} assets · ${leaves.length - assetCount} checks`,
        outputName: group ? memberName(groupOutput(id, tree)) : undefined,
      }
    })
  return { entries, external: [...external] }
}
