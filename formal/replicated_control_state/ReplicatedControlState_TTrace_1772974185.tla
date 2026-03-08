---- MODULE ReplicatedControlState_TTrace_1772974185 ----
EXTENDS Sequences, TLCExt, ReplicatedControlState, Toolbox, Naturals, TLC, ReplicatedControlState_TEConstants

_expression ==
    LET ReplicatedControlState_TEExpression == INSTANCE ReplicatedControlState_TEExpression
    IN ReplicatedControlState_TEExpression!expression
----

_trace ==
    LET ReplicatedControlState_TETrace == INSTANCE ReplicatedControlState_TETrace
    IN ReplicatedControlState_TETrace!trace
----

_inv ==
    ~(
        TLCGet("level") = Len(_TETrace)
        /\
        applied = ((d1 :> 3 @@ d2 :> 2))
        /\
        pending = ({})
        /\
        seen = ((d1 :> {1, 2, 3} @@ d2 :> {1, 2, 3}))
    )
----

_init ==
    /\ pending = _TETrace[1].pending
    /\ seen = _TETrace[1].seen
    /\ applied = _TETrace[1].applied
----

_next ==
    /\ \E i,j \in DOMAIN _TETrace:
        /\ \/ /\ j = i + 1
              /\ i = TLCGet("level")
        /\ pending  = _TETrace[i].pending
        /\ pending' = _TETrace[j].pending
        /\ seen  = _TETrace[i].seen
        /\ seen' = _TETrace[j].seen
        /\ applied  = _TETrace[i].applied
        /\ applied' = _TETrace[j].applied

\* Uncomment the ASSUME below to write the states of the error trace
\* to the given file in Json format. Note that you can pass any tuple
\* to `JsonSerialize`. For example, a sub-sequence of _TETrace.
    \* ASSUME
    \*     LET J == INSTANCE Json
    \*         IN J!JsonSerialize("ReplicatedControlState_TTrace_1772974185.json", _TETrace)

=============================================================================

 Note that you can extract this module `ReplicatedControlState_TEExpression`
  to a dedicated file to reuse `expression` (the module in the 
  dedicated `ReplicatedControlState_TEExpression.tla` file takes precedence 
  over the module `ReplicatedControlState_TEExpression` below).

---- MODULE ReplicatedControlState_TEExpression ----
EXTENDS Sequences, TLCExt, ReplicatedControlState, Toolbox, Naturals, TLC, ReplicatedControlState_TEConstants

expression == 
    [
        \* To hide variables of the `ReplicatedControlState` spec from the error trace,
        \* remove the variables below.  The trace will be written in the order
        \* of the fields of this record.
        pending |-> pending
        ,seen |-> seen
        ,applied |-> applied
        
        \* Put additional constant-, state-, and action-level expressions here:
        \* ,_stateNumber |-> _TEPosition
        \* ,_pendingUnchanged |-> pending = pending'
        
        \* Format the `pending` variable as Json value.
        \* ,_pendingJson |->
        \*     LET J == INSTANCE Json
        \*     IN J!ToJson(pending)
        
        \* Lastly, you may build expressions over arbitrary sets of states by
        \* leveraging the _TETrace operator.  For example, this is how to
        \* count the number of times a spec variable changed up to the current
        \* state in the trace.
        \* ,_pendingModCount |->
        \*     LET F[s \in DOMAIN _TETrace] ==
        \*         IF s = 1 THEN 0
        \*         ELSE IF _TETrace[s].pending # _TETrace[s-1].pending
        \*             THEN 1 + F[s-1] ELSE F[s-1]
        \*     IN F[_TEPosition - 1]
    ]

=============================================================================



Parsing and semantic processing can take forever if the trace below is long.
 In this case, it is advised to uncomment the module below to deserialize the
 trace from a generated binary file.

\*
\*---- MODULE ReplicatedControlState_TETrace ----
\*EXTENDS IOUtils, ReplicatedControlState, TLC, ReplicatedControlState_TEConstants
\*
\*trace == IODeserialize("ReplicatedControlState_TTrace_1772974185.bin", TRUE)
\*
\*=============================================================================
\*

---- MODULE ReplicatedControlState_TETrace ----
EXTENDS ReplicatedControlState, TLC, ReplicatedControlState_TEConstants

trace == 
    <<
    ([applied |-> (d1 :> 0 @@ d2 :> 0),pending |-> {<<d1, 1>>, <<d1, 2>>, <<d1, 3>>, <<d2, 1>>, <<d2, 2>>, <<d2, 3>>},seen |-> (d1 :> {} @@ d2 :> {})]),
    ([applied |-> (d1 :> 1 @@ d2 :> 0),pending |-> {<<d1, 2>>, <<d1, 3>>, <<d2, 1>>, <<d2, 2>>, <<d2, 3>>},seen |-> (d1 :> {1} @@ d2 :> {})]),
    ([applied |-> (d1 :> 2 @@ d2 :> 0),pending |-> {<<d1, 3>>, <<d2, 1>>, <<d2, 2>>, <<d2, 3>>},seen |-> (d1 :> {1, 2} @@ d2 :> {})]),
    ([applied |-> (d1 :> 3 @@ d2 :> 0),pending |-> {<<d2, 1>>, <<d2, 2>>, <<d2, 3>>},seen |-> (d1 :> {1, 2, 3} @@ d2 :> {})]),
    ([applied |-> (d1 :> 3 @@ d2 :> 1),pending |-> {<<d2, 2>>, <<d2, 3>>},seen |-> (d1 :> {1, 2, 3} @@ d2 :> {1})]),
    ([applied |-> (d1 :> 3 @@ d2 :> 3),pending |-> {<<d2, 2>>},seen |-> (d1 :> {1, 2, 3} @@ d2 :> {1, 3})]),
    ([applied |-> (d1 :> 3 @@ d2 :> 2),pending |-> {},seen |-> (d1 :> {1, 2, 3} @@ d2 :> {1, 2, 3})])
    >>
----


=============================================================================

---- MODULE ReplicatedControlState_TEConstants ----
EXTENDS ReplicatedControlState

CONSTANTS d1, d2

=============================================================================

---- CONFIG ReplicatedControlState_TTrace_1772974185 ----
CONSTANTS
    Devices = { d1 , d2 }
    Stamps = { 1 , 2 , 3 }
    DeletedStamps = { 3 }
    UseMaxStampResolution = FALSE
    d2 = d2
    d1 = d1

INVARIANT
    _inv

CHECK_DEADLOCK
    \* CHECK_DEADLOCK off because of PROPERTY or INVARIANT above.
    FALSE

INIT
    _init

NEXT
    _next

CONSTANT
    _TETrace <- _trace

ALIAS
    _expression
=============================================================================
\* Generated on Sun Mar 08 14:49:46 EET 2026