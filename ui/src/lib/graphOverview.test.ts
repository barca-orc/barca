import { describe, it, expect } from 'vitest'
import { graphOverview, graphState, groupState } from './graphOverview'
import type { AssetSummary, NodeState } from './types'
const chain = (): AssetSummary[] => ['a','b','c','d','e'].map((id,i,ids) => ({id,kind:'asset',freshness:{type:'Always'},env:[],inputs:i ? [ids[i-1]!] : []}))
const state = (cache: NodeState['cache']['state'], failed=false): NodeState => ({id:'a',name:'a',kind:'asset',inputs:[],partitioned:false,cache:{state:cache,reason:'no_record',detail:'reason'},last_materialization:failed ? {status:'failed',created_at:'today',elapsed_seconds:null,run_hash:null,artifact:null,format:null,size_bytes:null,error:'boom'} : null,shape:null,env:[],durations:null,next_run:null})
describe('graph overview', () => {
  it('rewires chains without hiding source or output', () => {
    const result=graphOverview(chain()); const id=Object.keys(result.groups)[0]!
    expect(result.groups[id]).toEqual(['b','c','d'])
    expect(result.assets.map(a=>[a.id,a.inputs])).toEqual([['a',[]],[id,['a']],['e',[id]]])
    expect(graphOverview(chain(),new Set(),new Set([id])).assets).toEqual(chain())
    expect(graphOverview(chain(),new Set(),new Set(),'c').assets).toEqual(chain())
  })
  it('preserves partition boundaries, tasks, schedules, branches and joins', () => {
    expect(graphOverview(chain(),new Set(['c'])).groups).toEqual({})
    for(const kind of ['task','sensor'] as const) { const a=chain(); a[2]!.kind=kind; expect(graphOverview(a).groups).toEqual({}) }
    const scheduled=chain(); scheduled[2]!.freshness={type:'Schedule',value:'0 * * * *'}; expect(graphOverview(scheduled).groups).toEqual({})
    const a=chain(); a.push({...a[4]!,id:'branch',inputs:['c']}); expect(graphOverview(a).groups).toEqual({})
    const join=chain(); join[2]!.inputs=['a','b']; expect(Object.values(graphOverview(join).groups).flat()).not.toContain('c')
  })
  it('retains single intermediates, external boundaries and cycles', () => {
    expect(graphOverview(chain().slice(0,3)).groups).toEqual({})
    const a=chain().slice(0,3); a[0]!.inputs=['external']; expect(Object.values(graphOverview(a).groups).flat()).not.toContain('a')
    a[0]!.inputs=['c']; expect(graphOverview(a).assets).toEqual(a)
  })
})
describe('graph colors', () => {
  it.each([['cached','success'],['stale','warning'],['partial','warning'],['never_run','skipped'],['unknown','skipped'],['always_runs','skipped']] as const)('%s maps to %s',(cache,status)=>expect(graphState(state(cache)).status).toBe(status))
  it('latest failure overrides a cached artifact',()=>expect(graphState(state('cached',true)).status).toBe('failed'))
  it('groups expose failures and never report success with unknown members',()=>{
    const states={a:graphState(state('cached')),b:graphState(state('stale')),c:graphState(state('cached',true))}
    expect(groupState(['a','b','c'],states)).toMatchObject({status:'failed',label:'1 last attempt failed · 1 stale · 1 cached'})
    expect(groupState(['a','missing'],states).status).toBe('skipped')
    expect(groupState(['a'],states).status).toBe('success')
  })
})
