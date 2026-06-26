using System;
using GitAiVS.Services;
using Microsoft.VisualStudio.TestTools.UnitTesting;

namespace GitAiVS.Tests
{
    [TestClass]
    public class BinaryResolverTests
    {
        [TestMethod]
        public void ParseVersionReturnsCorrectVersionForStandardFormat()
        {
            var version = BinaryResolver.ParseVersion("1.0.39");
            Assert.IsNotNull(version);
            Assert.AreEqual(new Version(1, 0, 39), version);
        }

        [TestMethod]
        public void ParseVersionHandlesDebugSuffix()
        {
            var version = BinaryResolver.ParseVersion("1.0.39 (debug)");
            Assert.IsNotNull(version);
            Assert.AreEqual(new Version(1, 0, 39), version);
        }

        [TestMethod]
        public void ParseVersionHandlesPreReleaseDash()
        {
            var version = BinaryResolver.ParseVersion("1.5.3-beta.1");
            Assert.IsNotNull(version);
            Assert.AreEqual(new Version(1, 5, 3), version);
        }

        [TestMethod]
        public void ParseVersionHandlesBuildMetadataPlus()
        {
            var version = BinaryResolver.ParseVersion("2.1.0+build.123");
            Assert.IsNotNull(version);
            Assert.AreEqual(new Version(2, 1, 0), version);
        }

        [TestMethod]
        public void ParseVersionReturnsNullForTooFewSegments()
        {
            Assert.IsNull(BinaryResolver.ParseVersion("1.0"));
        }

        [TestMethod]
        public void ParseVersionReturnsNullForEmptyString()
        {
            Assert.IsNull(BinaryResolver.ParseVersion(""));
        }

        [TestMethod]
        public void ParseVersionReturnsNullForGarbage()
        {
            Assert.IsNull(BinaryResolver.ParseVersion("not-a-version"));
        }

        [TestMethod]
        public void ParseVersionReturnsNullForNonNumericSegments()
        {
            Assert.IsNull(BinaryResolver.ParseVersion("a.b.c"));
        }

        [TestMethod]
        public void ParseVersionHandlesLeadingWhitespace()
        {
            var version = BinaryResolver.ParseVersion("  1.0.23  ");
            Assert.IsNotNull(version);
            Assert.AreEqual(new Version(1, 0, 23), version);
        }
    }
}
