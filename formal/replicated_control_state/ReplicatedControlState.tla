---- MODULE ReplicatedControlState ----
EXTENDS FiniteSets, Naturals

CONSTANTS
    Devices,
    Stamps,
    DeletedStamps,
    UseMaxStampResolution

ASSUME Devices # {}
ASSUME Stamps # {}
ASSUME DeletedStamps \subseteq Stamps
ASSUME UseMaxStampResolution \in BOOLEAN

Pairs == {<<d, s>> : d \in Devices, s \in Stamps}

MaxSeen(S) ==
    IF S = {} THEN 0
    ELSE CHOOSE s \in S: \A t \in S: t <= s

VARIABLES
    seen,
    applied,
    pending

vars == <<seen, applied, pending>>

Init ==
    /\ seen = [d \in Devices |-> {}]
    /\ applied = [d \in Devices |-> 0]
    /\ pending = Pairs

Deliver(d, s) ==
    /\ <<d, s>> \in pending
    /\ seen' = [seen EXCEPT ![d] = @ \cup {s}]
    /\ applied' =
        [applied EXCEPT ![d] =
            IF UseMaxStampResolution
            THEN MaxSeen(seen'[d])
            ELSE s
        ]
    /\ pending' = pending \ {<<d, s>>}

Replay(d, s) ==
    /\ d \in Devices
    /\ s \in seen[d]
    /\ seen' = seen
    /\ applied' =
        [applied EXCEPT ![d] =
            IF UseMaxStampResolution
            THEN MaxSeen(seen[d])
            ELSE s
        ]
    /\ pending' = pending

Next ==
    \E d \in Devices:
        \E s \in Stamps:
            Deliver(d, s) \/ Replay(d, s)

Spec == Init /\ [][Next]_vars

AllSeen ==
    \A d \in Devices: seen[d] = Stamps

WinningStamp == MaxSeen(Stamps)

Converged ==
    \A d \in Devices: applied[d] = WinningStamp

DeleteWins ==
    AllSeen /\ WinningStamp \in DeletedStamps => (\A d \in Devices: applied[d] = WinningStamp)

NoStaleResurrection ==
    AllSeen => Converged

THEOREM Spec => []DeleteWins
=============================================================================
