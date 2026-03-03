---- MODULE SessionLifecycle ----
EXTENDS FiniteSets, Naturals

CONSTANTS
    Devices,
    MaxInactive,
    MaxRelayCopies,
    InitialAuthorized,
    ReplacementAuthorized,
    BugAllowMultipleActive,
    BugAllowRevokedDelivery

ASSUME Devices # {}
ASSUME MaxInactive \in Nat
ASSUME MaxRelayCopies \in Nat \ {0}
ASSUME InitialAuthorized \subseteq Devices
ASSUME ReplacementAuthorized \subseteq Devices

VARIABLES
    authorized,
    revoked,
    replaced,
    cleanupDone,
    activeCount,
    inactiveCount,
    sent,
    msgQ,
    relayUp,
    relayBag,
    delivered,
    deliveredWhileRevoked

vars ==
    <<authorized, revoked, replaced, cleanupDone,
      activeCount, inactiveCount, sent, msgQ,
      relayUp, relayBag, delivered, deliveredWhileRevoked>>

ZeroBag ==
    [d \in Devices |-> 0]

Init ==
    /\ authorized = InitialAuthorized
    /\ revoked = {}
    /\ replaced = FALSE
    /\ cleanupDone = {}
    /\ activeCount = [d \in Devices |-> IF d \in InitialAuthorized THEN 1 ELSE 0]
    /\ inactiveCount = [d \in Devices |-> 0]
    /\ sent = [d \in Devices |-> FALSE]
    /\ msgQ = {}
    /\ relayUp = TRUE
    /\ relayBag = ZeroBag
    /\ delivered = {}
    /\ deliveredWhileRevoked = {}

BagAdd(b, d) ==
    [b EXCEPT ![d] = IF @ >= MaxRelayCopies THEN @ ELSE @ + 1]

BagDec(b, d) ==
    [b EXCEPT ![d] = IF @ = 0 THEN 0 ELSE @ - 1]

EstablishSession(d) ==
    /\ d \in authorized
    /\ d \notin revoked
    /\ activeCount[d] = 0
    /\ activeCount' = [activeCount EXCEPT ![d] = 1]
    /\ UNCHANGED <<
        authorized, revoked, replaced, cleanupDone,
        inactiveCount, sent, msgQ,
        relayUp, relayBag, delivered, deliveredWhileRevoked
      >>

RotateSession(d) ==
    /\ d \in authorized
    /\ d \notin revoked
    /\ activeCount[d] > 0
    /\ IF BugAllowMultipleActive
          THEN /\ activeCount' = [activeCount EXCEPT ![d] = @ + 1]
               /\ inactiveCount' = inactiveCount
          ELSE /\ activeCount' = [activeCount EXCEPT ![d] = 1]
               /\ inactiveCount' =
                   [inactiveCount EXCEPT ![d] = IF @ >= MaxInactive THEN MaxInactive ELSE @ + 1]
    /\ UNCHANGED <<
        authorized, revoked, replaced, cleanupDone,
        sent, msgQ,
        relayUp, relayBag, delivered, deliveredWhileRevoked
      >>

PromoteInactive(d) ==
    /\ d \in authorized
    /\ d \notin revoked
    /\ inactiveCount[d] > 0
    /\ inactiveCount' = [inactiveCount EXCEPT ![d] = @ - 1]
    /\ IF BugAllowMultipleActive
          THEN activeCount' = [activeCount EXCEPT ![d] = @ + 1]
          ELSE activeCount' = [activeCount EXCEPT ![d] = 1]
    /\ UNCHANGED <<
        authorized, revoked, replaced, cleanupDone,
        sent, msgQ,
        relayUp, relayBag, delivered, deliveredWhileRevoked
      >>

QueueSend(d) ==
    /\ d \in authorized
    /\ d \notin revoked
    /\ activeCount[d] > 0
    /\ ~sent[d]
    /\ sent' = [sent EXCEPT ![d] = TRUE]
    /\ msgQ' = msgQ \cup {d}
    /\ UNCHANGED <<
        authorized, revoked, replaced, cleanupDone,
        activeCount, inactiveCount,
        relayUp, relayBag, delivered, deliveredWhileRevoked
      >>

Flush(d) ==
    /\ d \in msgQ
    /\ d \in authorized
    /\ d \notin revoked
    /\ activeCount[d] > 0
    /\ relayUp
    /\ relayBag[d] < MaxRelayCopies
    /\ relayBag' = BagAdd(relayBag, d)
    /\ UNCHANGED <<
        authorized, revoked, replaced, cleanupDone,
        activeCount, inactiveCount, sent, msgQ,
        relayUp, delivered, deliveredWhileRevoked
      >>

RelayDeliver(d) ==
    /\ relayUp
    /\ relayBag[d] > 0
    /\ (BugAllowRevokedDelivery \/ d \notin revoked)
    /\ relayBag' = BagDec(relayBag, d)
    /\ msgQ' = msgQ \ {d}
    /\ delivered' = delivered \cup {d}
    /\ deliveredWhileRevoked' =
        IF d \in revoked \/ d \in cleanupDone
            THEN deliveredWhileRevoked \cup {d}
            ELSE deliveredWhileRevoked
    /\ UNCHANGED <<
        authorized, revoked, replaced, cleanupDone,
        activeCount, inactiveCount, sent,
        relayUp
      >>

RelayDrop(d) ==
    /\ relayBag[d] > 0
    /\ relayBag' = BagDec(relayBag, d)
    /\ UNCHANGED <<
        authorized, revoked, replaced, cleanupDone,
        activeCount, inactiveCount, sent, msgQ,
        relayUp, delivered, deliveredWhileRevoked
      >>

RelayDuplicate(d) ==
    /\ relayBag[d] > 0
    /\ relayBag[d] < MaxRelayCopies
    /\ relayBag' = BagAdd(relayBag, d)
    /\ UNCHANGED <<
        authorized, revoked, replaced, cleanupDone,
        activeCount, inactiveCount, sent, msgQ,
        relayUp, delivered, deliveredWhileRevoked
      >>

RelayPartition ==
    /\ relayUp
    /\ relayUp' = FALSE
    /\ UNCHANGED <<
        authorized, revoked, replaced, cleanupDone,
        activeCount, inactiveCount, sent, msgQ,
        relayBag, delivered, deliveredWhileRevoked
      >>

RelayRecover ==
    /\ ~relayUp
    /\ relayUp' = TRUE
    /\ UNCHANGED <<
        authorized, revoked, replaced, cleanupDone,
        activeCount, inactiveCount, sent, msgQ,
        relayBag, delivered, deliveredWhileRevoked
      >>

RelayDelay ==
    /\ \E d \in Devices: relayBag[d] > 0
    /\ UNCHANGED vars

AppKeysRevoke ==
    /\ ~replaced
    /\ ReplacementAuthorized # authorized
    /\ LET removed == authorized \ ReplacementAuthorized IN
       /\ authorized' = ReplacementAuthorized
       /\ revoked' = (revoked \cup removed) \ ReplacementAuthorized
       /\ cleanupDone' = cleanupDone \ ReplacementAuthorized
    /\ replaced' = TRUE
    /\ UNCHANGED <<
        activeCount, inactiveCount, sent, msgQ,
        relayUp, relayBag, delivered, deliveredWhileRevoked
      >>

CleanupRevoked(d) ==
    /\ d \in revoked
    /\ d \notin cleanupDone
    /\ activeCount' = [activeCount EXCEPT ![d] = 0]
    /\ inactiveCount' = [inactiveCount EXCEPT ![d] = 0]
    /\ relayBag' = [relayBag EXCEPT ![d] = 0]
    /\ msgQ' = msgQ \ {d}
    /\ cleanupDone' = cleanupDone \cup {d}
    /\ UNCHANGED <<
        authorized, revoked, replaced, sent,
        relayUp, delivered, deliveredWhileRevoked
      >>

Stutter ==
    UNCHANGED vars

RelayEventuallyRecovers ==
    <>[]relayUp

Next ==
    \/ \E d \in Devices: EstablishSession(d)
    \/ \E d \in Devices: RotateSession(d)
    \/ \E d \in Devices: PromoteInactive(d)
    \/ \E d \in Devices: QueueSend(d)
    \/ \E d \in Devices: Flush(d)
    \/ \E d \in Devices: RelayDeliver(d)
    \/ \E d \in Devices: RelayDrop(d)
    \/ \E d \in Devices: RelayDuplicate(d)
    \/ RelayPartition
    \/ RelayRecover
    \/ RelayDelay
    \/ AppKeysRevoke
    \/ \E d \in Devices: CleanupRevoked(d)
    \/ Stutter

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ WF_vars(AppKeysRevoke)
    /\ \A d \in Devices: WF_vars(EstablishSession(d))
    /\ \A d \in Devices: WF_vars(QueueSend(d))
    /\ \A d \in Devices: WF_vars(Flush(d))
    /\ \A d \in Devices: SF_vars(RelayDeliver(d))
    /\ \A d \in Devices: WF_vars(CleanupRevoked(d))

SpecUnderRecovery ==
    /\ Spec
    /\ RelayEventuallyRecovers

OneActiveSessionPerDevice ==
    \A d \in Devices: activeCount[d] <= 1

InactiveQueueBounded ==
    \A d \in Devices: inactiveCount[d] <= MaxInactive

NoDeliverToRevokedAfterCleanup ==
    deliveredWhileRevoked = {}

NoStateForRevokedAfterCleanup ==
    \A d \in cleanupDone:
        /\ activeCount[d] = 0
        /\ inactiveCount[d] = 0
        /\ relayBag[d] = 0
        /\ d \notin msgQ

SentAuthorizedEventuallyDeliveredUnderRecovery ==
    \A d \in Devices:
        [](sent[d] /\ d \in authorized /\ d \notin revoked => <> (d \in delivered \/ d \in revoked))

RevokedEventuallyPurged ==
    \A d \in Devices:
        [](d \in revoked => <> (d \in cleanupDone))

====
