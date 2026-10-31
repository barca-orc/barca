import { describe, it, expect } from 'vitest'
import { graphState } from './graphState'
import type { NodeState } from './types'
const state = (cache: NodeState['cache']['state'], failed=false): NodeState => ({id:'a',name:'a',kind:'asset',inputs:[],partitioned:false,cache:{state:cache,reason:'no_record',detail:'reason'},last_materialization:failed ? {status:'failed',created_at:'today',elapsed_seconds:null,run_hash:null,artifact:null,format:null,size_bytes:null,error:'boom'} : null,shape:null,env:[],durations:null,next_run:null, last_success_at: null, failing_since: null, consecutive_failures: 0,running:null})
describe('graph colors', () => {
  it.each([['cached','success'],['stale','warning'],['partial','warning'],['never_run','skipped'],['unknown','skipped'],['always_runs','skipped'],['unsynced','warning']] as const)('%s maps to %s',(cache,status)=>expect(graphState(state(cache)).status).toBe(status))
  it('latest failure overrides a cached artifact',()=>expect(graphState(state('cached',true)).status).toBe('failed'))
  it('a blocked node is stale-colored and says so',()=>expect(graphState(state('blocked'))).toMatchObject({status:'warning',label:'Blocked',hint:'reason'}))
})
