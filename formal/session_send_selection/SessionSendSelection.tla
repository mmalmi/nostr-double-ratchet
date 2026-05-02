---- MODULE SessionSendSelection ----
EXTENDS FiniteSets, Naturals

CONSTANTS
    Sessions,
    ActiveSession,
    SendableSessions,
    ReceivableSessions,
    HighReceiveSessions,
    HighPreviousSendingSessions,
    HighSendingSessions,
    BugIgnoreActiveReceiveBonus,
    BugIgnorePreviousSendingCount

ASSUME Sessions # {}
ASSUME ActiveSession \in Sessions
ASSUME SendableSessions \subseteq Sessions
ASSUME SendableSessions # {}
ASSUME ReceivableSessions \subseteq Sessions
ASSUME HighReceiveSessions \subseteq Sessions
ASSUME HighPreviousSendingSessions \subseteq Sessions
ASSUME HighSendingSessions \subseteq Sessions
ASSUME BugIgnoreActiveReceiveBonus \in BOOLEAN
ASSUME BugIgnorePreviousSendingCount \in BOOLEAN

Directionality(s) ==
    IF s \in SendableSessions /\ s \in ReceivableSessions
        THEN 3
    ELSE IF s \in SendableSessions
        THEN 2
    ELSE IF s \in ReceivableSessions
        THEN 1
    ELSE 0

ActiveReceiveBonus(s) ==
    IF ~BugIgnoreActiveReceiveBonus /\ s = ActiveSession /\ s \in ReceivableSessions
        THEN 1
        ELSE 0

RawPreviousCount(s) ==
    IF s \in HighPreviousSendingSessions THEN 5 ELSE 0

PreviousCount(s) ==
    IF BugIgnorePreviousSendingCount THEN 0 ELSE RawPreviousCount(s)

ReceivingCount(s) ==
    IF s \in HighReceiveSessions THEN 4 ELSE 1

SendingCount(s) ==
    IF s \in HighSendingSessions THEN 1 ELSE 0

Priority(s) ==
    <<Directionality(s),
      ActiveReceiveBonus(s),
      ReceivingCount(s),
      PreviousCount(s),
      SendingCount(s)>>

LexGeq(left, right) ==
    \/ left[1] > right[1]
    \/ left[1] = right[1] /\ left[2] > right[2]
    \/ left[1] = right[1] /\ left[2] = right[2] /\ left[3] > right[3]
    \/ left[1] = right[1] /\ left[2] = right[2] /\ left[3] = right[3] /\ left[4] > right[4]
    \/ left[1] = right[1] /\ left[2] = right[2] /\ left[3] = right[3] /\ left[4] = right[4] /\ left[5] >= right[5]

Best(s) ==
    /\ s \in SendableSessions
    /\ \A other \in SendableSessions:
        LexGeq(Priority(s), Priority(other))

VARIABLE selected

vars == <<selected>>

Init ==
    /\ selected \in SendableSessions
    /\ Best(selected)

Next ==
    UNCHANGED vars

Spec ==
    /\ Init
    /\ [][Next]_vars

SelectedIsBest ==
    Best(selected)

ActiveBidirectionalSendStaysActive ==
    ActiveSession \in SendableSessions /\ ActiveSession \in ReceivableSessions
        => selected = ActiveSession

PreviousChainCountBreaksReceiveTie ==
    \A old \in SendableSessions:
        \A newer \in SendableSessions:
            (/\ old # newer
             /\ old # ActiveSession
             /\ newer # ActiveSession
             /\ Directionality(old) = Directionality(newer)
             /\ ReceivingCount(old) = ReceivingCount(newer)
             /\ RawPreviousCount(newer) > RawPreviousCount(old)
             /\ SendingCount(old) > SendingCount(newer))
                => selected # old

====
