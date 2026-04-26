---- MODULE DirectMessageSubscriptions ----
EXTENDS FiniteSets, Naturals

CONSTANTS
    Authors,
    InitialMessageAuthors,
    UpdatedMessageAuthors,
    BugSessionManagerOwnsDirectSubscriptions,
    BugRuntimeSkipsSync,
    BugRuntimeLeavesStaleSubscriptions

ASSUME Authors # {}
ASSUME InitialMessageAuthors \subseteq Authors
ASSUME UpdatedMessageAuthors \subseteq Authors

VARIABLES
    managerAuthors,
    runtimeSubscriptionAuthors,
    managerDirectSubscriptionAuthors,
    needsSync,
    changeDone,
    relayUp,
    relayBag,
    published,
    delivered

vars ==
    <<managerAuthors, runtimeSubscriptionAuthors, managerDirectSubscriptionAuthors,
      needsSync, changeDone, relayUp, relayBag, published, delivered>>

Init ==
    /\ managerAuthors = InitialMessageAuthors
    /\ runtimeSubscriptionAuthors = {}
    /\ managerDirectSubscriptionAuthors =
        IF BugSessionManagerOwnsDirectSubscriptions THEN InitialMessageAuthors ELSE {}
    /\ needsSync = (InitialMessageAuthors # {})
    /\ changeDone = FALSE
    /\ relayUp = TRUE
    /\ relayBag = {}
    /\ published = {}
    /\ delivered = {}

SessionStateChanges ==
    /\ ~changeDone
    /\ ~needsSync
    /\ managerAuthors' = UpdatedMessageAuthors
    /\ managerDirectSubscriptionAuthors' =
        IF BugSessionManagerOwnsDirectSubscriptions THEN UpdatedMessageAuthors ELSE {}
    /\ needsSync' = TRUE
    /\ changeDone' = TRUE
    /\ UNCHANGED <<runtimeSubscriptionAuthors, relayUp, relayBag, published, delivered>>

SyncRuntimeSubscription ==
    /\ needsSync
    /\ ~BugRuntimeSkipsSync
    /\ runtimeSubscriptionAuthors' =
        IF BugRuntimeLeavesStaleSubscriptions
            THEN runtimeSubscriptionAuthors \cup managerAuthors
            ELSE managerAuthors
    /\ needsSync' = FALSE
    /\ UNCHANGED <<
        managerAuthors, managerDirectSubscriptionAuthors, changeDone,
        relayUp, relayBag, published, delivered
      >>

PublishInbound(a) ==
    /\ a \in managerAuthors
    /\ a \notin published
    /\ relayBag' = relayBag \cup {a}
    /\ published' = published \cup {a}
    /\ UNCHANGED <<
        managerAuthors, runtimeSubscriptionAuthors, managerDirectSubscriptionAuthors,
        needsSync, changeDone, relayUp, delivered
      >>

RelayDeliver(a) ==
    /\ relayUp
    /\ a \in relayBag
    /\ a \in runtimeSubscriptionAuthors
    /\ relayBag' = relayBag \ {a}
    /\ delivered' = delivered \cup {a}
    /\ UNCHANGED <<
        managerAuthors, runtimeSubscriptionAuthors, managerDirectSubscriptionAuthors,
        needsSync, changeDone, relayUp, published
      >>

RelayPartition ==
    /\ relayUp
    /\ relayUp' = FALSE
    /\ UNCHANGED <<
        managerAuthors, runtimeSubscriptionAuthors, managerDirectSubscriptionAuthors,
        needsSync, changeDone, relayBag, published, delivered
      >>

RelayRecover ==
    /\ ~relayUp
    /\ relayUp' = TRUE
    /\ UNCHANGED <<
        managerAuthors, runtimeSubscriptionAuthors, managerDirectSubscriptionAuthors,
        needsSync, changeDone, relayBag, published, delivered
      >>

RelayDelay ==
    /\ relayBag # {}
    /\ UNCHANGED vars

Stutter ==
    UNCHANGED vars

RelayEventuallyRecovers ==
    <>[]relayUp

Next ==
    \/ SessionStateChanges
    \/ SyncRuntimeSubscription
    \/ \E a \in Authors: PublishInbound(a)
    \/ \E a \in Authors: RelayDeliver(a)
    \/ RelayPartition
    \/ RelayRecover
    \/ RelayDelay
    \/ Stutter

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ WF_vars(SessionStateChanges)
    /\ WF_vars(SyncRuntimeSubscription)
    /\ \A a \in Authors: WF_vars(PublishInbound(a))
    /\ \A a \in Authors: SF_vars(RelayDeliver(a))

SpecUnderRecovery ==
    /\ Spec
    /\ RelayEventuallyRecovers

SessionManagerDoesNotOwnDirectMessageSubscriptions ==
    managerDirectSubscriptionAuthors = {}

CleanRuntimeSubscriptionMirrorsSessionManager ==
    ~needsSync => runtimeSubscriptionAuthors = managerAuthors

RuntimeSubscriptionAuthorsAreKnown ==
    runtimeSubscriptionAuthors \subseteq Authors

RuntimeSubscriptionEventuallyClean ==
    [](needsSync => <> ~needsSync)

TrackedInboundEventuallyDeliveredUnderRecovery ==
    \A a \in Authors:
        [](a \in published /\ a \in managerAuthors => <> (a \in delivered \/ a \notin managerAuthors))

RemovedAuthorsEventuallyUnsubscribed ==
    \A a \in InitialMessageAuthors \ UpdatedMessageAuthors:
        [](changeDone => <> (a \notin runtimeSubscriptionAuthors))

====
