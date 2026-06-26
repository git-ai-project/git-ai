using GitAiVS.Services;
using Microsoft.VisualStudio.TestTools.UnitTesting;

namespace GitAiVS.Tests
{
    [TestClass]
    public class GitRepoResolverTests
    {
        [TestMethod]
        public void ToRelativePathStripsWorkspacePrefix()
        {
            var result = GitRepoResolver.ToRelativePath(
                @"C:\Users\dev\project\src\Program.cs",
                @"C:\Users\dev\project");
            Assert.AreEqual(@"src\Program.cs", result);
        }

        [TestMethod]
        public void ToRelativePathHandlesTrailingSeparator()
        {
            var result = GitRepoResolver.ToRelativePath(
                @"C:\Users\dev\project\src\Program.cs",
                @"C:\Users\dev\project\");
            Assert.AreEqual(@"src\Program.cs", result);
        }

        [TestMethod]
        public void ToRelativePathReturnsAbsoluteWhenNotUnderRoot()
        {
            var result = GitRepoResolver.ToRelativePath(
                @"C:\other\file.txt",
                @"C:\Users\dev\project");
            Assert.AreEqual(@"C:\other\file.txt", result);
        }

        [TestMethod]
        public void ToRelativePathReturnsAbsoluteForSiblingWithSharedPrefix()
        {
            var result = GitRepoResolver.ToRelativePath(
                @"C:\Users\dev\projects\file.txt",
                @"C:\Users\dev\project");
            Assert.AreEqual(@"C:\Users\dev\projects\file.txt", result);
        }

        [TestMethod]
        public void ToRelativePathHandlesUnixPaths()
        {
            var result = GitRepoResolver.ToRelativePath(
                "/home/dev/project/src/main.rs",
                "/home/dev/project");
            Assert.AreEqual("src/main.rs", result);
        }

        [TestMethod]
        public void ToRelativePathHandlesFileAtRoot()
        {
            var result = GitRepoResolver.ToRelativePath(
                @"C:\Users\dev\project\README.md",
                @"C:\Users\dev\project");
            Assert.AreEqual("README.md", result);
        }
    }
}
