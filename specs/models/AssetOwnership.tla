--------------------------- MODULE AssetOwnership ---------------------------
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Keys, Workers, Requests, MaxRevision, ConcurrentVersions
ASSUME /\ Keys # {} /\ Workers # {} /\ Requests # {}
       /\ MaxRevision \in Nat \ {0} /\ ConcurrentVersions \in BOOLEAN

Revisions == 1..MaxRevision
Jobs == Keys \X Revisions
NoJob == <<>>
Work == [job : Jobs, worker : Workers]

VARIABLES observed, selected, priorSelection, completed, running, requests, protected
vars == <<observed, selected, priorSelection, completed, running, requests, protected>>

RunningJobs == {w.job : w \in running}
Latest == {<<k, observed[k]>> : k \in {key \in Keys : observed[key] > 0}}
Demanded == Latest \cup ({requests[r] : r \in Requests} \ {NoJob})
OtherNeeds(job, r) == job \in Latest \/ \E q \in Requests \ {r} : requests[q] = job

Init == /\ observed = [k \in Keys |-> 0]
        /\ selected = [k \in Keys |-> 0]
        /\ priorSelection = selected
        /\ completed = {}
        /\ running = {}
        /\ requests = [r \in Requests |-> NoJob]
        /\ protected = {}

Observe(k) ==
    /\ observed[k] < MaxRevision
    /\ observed' = [observed EXCEPT ![k] = @ + 1]
    /\ UNCHANGED <<selected, priorSelection, completed, running, requests, protected>>

Resolve(r, k) ==
    /\ requests[r] = NoJob /\ observed[k] > 0
    /\ requests' = [requests EXCEPT ![r] = <<k, observed[k]>>]
    /\ UNCHANGED <<observed, selected, priorSelection, completed, running, protected>>

Start(job, worker) ==
    /\ job \in Demanded \ completed
    /\ job \notin RunningJobs
    /\ worker \notin {w.worker : w \in running}
    /\ ConcurrentVersions \/ ~\E w \in running : w.job[1] = job[1]
    /\ running' = running \cup {[job |-> job, worker |-> worker]}
    /\ UNCHANGED <<observed, selected, priorSelection, completed, requests, protected>>

Complete(w) ==
    /\ w \in running
    /\ completed' = completed \cup {w.job}
    /\ priorSelection' = selected
    /\ selected' = IF w.job[2] > selected[w.job[1]]
                    THEN [selected EXCEPT ![w.job[1]] = w.job[2]]
                    ELSE selected
    /\ running' = running \ {w}
    /\ protected' = protected \ {w.job}
    /\ UNCHANGED <<observed, requests>>

Satisfy(r) ==
    /\ requests[r] \in completed
    /\ requests' = [requests EXCEPT ![r] = NoJob]
    /\ UNCHANGED <<observed, selected, priorSelection, completed, running, protected>>

Cancel(r) ==
    /\ requests[r] # NoJob
    /\ protected' = {job \in RunningJobs : \E q \in Requests \ {r} : requests[q] = job}
    /\ running' = {w \in running : w.job # requests[r] \/ OtherNeeds(w.job, r)}
    /\ requests' = [requests EXCEPT ![r] = NoJob]
    /\ UNCHANGED <<observed, selected, priorSelection, completed>>

Fail(w) ==
    /\ w \in running
    /\ running' = running \ {w}
    /\ protected' = protected \ {w.job}
    /\ UNCHANGED <<observed, selected, priorSelection, completed, requests>>

Crash ==
    /\ running # {}
    /\ running' = {} /\ protected' = {}
    /\ UNCHANGED <<observed, selected, priorSelection, completed, requests>>

Next == \/ \E k \in Keys : Observe(k)
        \/ \E r \in Requests, k \in Keys : Resolve(r, k)
        \/ \E job \in Jobs, worker \in Workers : Start(job, worker)
        \/ \E w \in running : Complete(w) \/ Fail(w)
        \/ \E r \in Requests : Satisfy(r) \/ Cancel(r)
        \/ Crash

Spec == Init /\ [][Next]_vars

TypeOK == /\ observed \in [Keys -> 0..MaxRevision]
          /\ selected \in [Keys -> 0..MaxRevision]
          /\ priorSelection \in [Keys -> 0..MaxRevision]
          /\ completed \subseteq Jobs /\ running \subseteq Work
          /\ requests \in [Requests -> Jobs \cup {NoJob}]
          /\ protected \subseteq Jobs
NoDuplicateWork == \A a, b \in running : a.job = b.job => a.worker = b.worker
WorkerBound == \A a, b \in running : a.worker = b.worker => a.job = b.job
SelectedIsCommitted == \A k \in Keys : selected[k] = 0 \/ <<k, selected[k]>> \in completed
NoRevisionRegression == \A k \in Keys : priorSelection[k] <= selected[k] /\ selected[k] <= observed[k]
CancelPreservesOthers == protected \subseteq RunningJobs
ConservativeVersions == ConcurrentVersions \/ \A a, b \in running : a.job[1] = b.job[1] => a = b
=============================================================================
